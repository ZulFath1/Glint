//! Media sessions: what is playing, from which app, and the buttons to
//! control it (CLAUDE.md section 8).
//!
//! This is the same Windows API that powers the media controls in the
//! volume flyout (`GlobalSystemMediaTransportControlsSessionManager`), so it
//! works with Spotify, browsers (YouTube), Media Player and most players.
//! There is one "session" per app (or per browser tab) that plays media.
//!
//! No drawing happens here (see `ui/music.rs`), and nothing ever waits:
//!
//! - **Events.** Windows calls our handlers on its own worker threads. A
//!   handler only posts [`WM_APP_MEDIA`] (with a "what changed" bit) to our
//!   window. The UI thread then reads the new state with the quick,
//!   synchronous getters (`GetPlaybackInfo`, `GetTimelineProperties`).
//! - **Async calls** (`…Async()`) get a `.when(…)` callback that runs later,
//!   also on a worker thread. It turns the answer into plain data (strings,
//!   pixels), sends it through a channel and posts a message, the same way
//!   the app list arrives (see `apps.rs`).
//! - **No leaks.** Every event subscription is a [`Subscription`] whose
//!   `Drop` removes it again, so replacing one can't forget the old one.
//! - **Zero CPU while hidden.** The timeline event (some players send it
//!   often) is only subscribed while the menu is open.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Instant;

use windows::Foundation::TypedEventHandler;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSession as Session,
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackControls as Controls,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Storage::Streams::IRandomAccessStreamWithContentType;
use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppPBGRA,
    IWICBitmapSource, IWICImagingFactory, WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant,
    WICBitmapPaletteTypeMedianCut, WICDecodeMetadataCacheOnDemand, WICRect,
};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
    IStream,
};
use windows::Win32::System::SystemInformation::GetSystemTimePreciseAsFileTime;
use windows::Win32::System::WinRT::CreateStreamOverRandomAccessStream;
use windows::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_APP};
use windows::core::{Error, GUID, Interface, Result};

use crate::config::Rgb;

/// Posted by the event handlers; wParam = the `CHANGED_*` bits.
pub const WM_APP_MEDIA: u32 = WM_APP + 9;
/// Posted once the session manager is ready (a moment after startup).
pub const WM_APP_MEDIA_READY: u32 = WM_APP + 10;
/// Posted when a title / artist / cover fetch finished.
pub const WM_APP_MEDIA_INFO: u32 = WM_APP + 11;

pub const CHANGED_SESSIONS: usize = 1;
pub const CHANGED_CURRENT: usize = 2;
pub const CHANGED_PLAYBACK: usize = 4;
pub const CHANGED_PROPERTIES: usize = 8;
pub const CHANGED_TIMELINE: usize = 16;

/// Timeline values are in "ticks" of 100 ns.
pub const TICKS_PER_SECOND: i64 = 10_000_000;

/// What the music card's buttons (and keys) ask for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCommand {
    Previous,
    PlayPause,
    Next,
    /// Jump to this position (ticks from the start of the track).
    Seek(i64),
    /// Show the next app that has media.
    SwitchSource,
}

/// An album cover, decoded straight to the size it is drawn at.
#[derive(Debug)]
pub struct Cover {
    /// Width and height in pixels.
    pub px: u32,
    /// Premultiplied BGRA, `px × px × 4` bytes.
    pub pixels: Vec<u8>,
    /// The cover's main color (for the optional album tint).
    pub color: Option<Rgb>,
}

/// Title, artist and cover of the shown session.
#[derive(Debug, Default)]
pub struct Info {
    pub title: String,
    pub artist: String,
    pub cover: Option<Cover>,
}

/// Everything the music card shows, as plain data.
#[derive(Debug, Clone)]
pub struct MusicView {
    /// Shared, so handing the view around doesn't copy the cover.
    pub info: Rc<Info>,
    pub playing: bool,
    pub can_previous: bool,
    pub can_play_pause: bool,
    pub can_next: bool,
    pub can_seek: bool,
    /// Ticks from the start of the track, as of `at`.
    pub position: i64,
    /// Length of the track in ticks (0 = unknown).
    pub length: i64,
    pub rate: f64,
    pub at: Instant,
    /// How many apps are playing or paused (2+ shows "Switch source").
    pub sources: usize,
    /// The shown app's AppUserModelID (to find its name and icon).
    pub source_id: String,
}

impl MusicView {
    /// The position right now (it keeps moving while playing).
    pub fn position_now(&self) -> i64 {
        if !self.playing {
            return self.position;
        }
        let ran = self.at.elapsed().as_secs_f64() * self.rate * TICKS_PER_SECOND as f64;
        clamp_position(self.position + ran as i64, self.length)
    }
}

// ---------------------------------------------------------------------------
// Pure helpers (unit tested)
// ---------------------------------------------------------------------------

/// What matters about a session's playback status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    Playing,
    Paused,
    /// Stopped, closed, opening... (not shown).
    Other,
}

impl State {
    fn active(self) -> bool {
        self != State::Other
    }
}

/// Which session to show: the user's pick (if it still plays or is
/// paused), else Windows' current session (same rule), else the first one
/// playing, else the first one paused. `None` = hide the card.
pub fn pick(states: &[State], current: Option<usize>, picked: Option<usize>) -> Option<usize> {
    let active = |i: &usize| states.get(*i).is_some_and(|s| s.active());
    picked
        .filter(active)
        .or(current.filter(active))
        .or_else(|| states.iter().position(|s| *s == State::Playing))
        .or_else(|| states.iter().position(|s| *s == State::Paused))
}

/// "Switch source": the next active session after `shown` (wrapping).
pub fn next_source(states: &[State], shown: usize) -> Option<usize> {
    let n = states.len();
    (1..n)
        .map(|step| (shown + step) % n)
        .find(|&i| states[i].active())
}

fn clamp_position(position: i64, length: i64) -> i64 {
    if length > 0 {
        position.clamp(0, length)
    } else {
        position.max(0)
    }
}

/// Where playback is now: the reported position, plus the time since it
/// was reported (while playing). All values are 100 ns ticks; `last_updated`
/// and `now` are UTC file times (0 = unknown).
pub fn current_position(
    position: i64,
    last_updated: i64,
    now: i64,
    playing: bool,
    rate: f64,
    length: i64,
) -> i64 {
    let ran = if playing && last_updated > 0 && now > last_updated {
        ((now - last_updated) as f64 * rate) as i64
    } else {
        0
    };
    clamp_position(position + ran, length)
}

/// "0:07", "3:25", "1:02:03".
pub fn format_time(ticks: i64) -> String {
    let total = ticks.max(0) / TICKS_PER_SECOND;
    let (h, m, s) = (total / 3600, total / 60 % 60, total % 60);
    if h > 0 {
        format!("{h}:{m:02}:{s:02}")
    } else {
        format!("{m}:{s:02}")
    }
}

/// The cover's main color: the average of its colorful pixels (HSV
/// saturation above 0.25, mostly opaque). `None` for a gray cover.
/// `bgra` is straight (not premultiplied) BGRA.
pub fn dominant_color(bgra: &[u8]) -> Option<Rgb> {
    let (mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32);
    for &[pb, pg, pr, pa] in bgra.as_chunks::<4>().0 {
        let max = pr.max(pg).max(pb);
        let min = pr.min(pg).min(pb);
        if pa < 128 || max == 0 {
            continue;
        }
        let saturation = f32::from(max - min) / f32::from(max);
        if saturation > 0.25 {
            r += u32::from(pr);
            g += u32::from(pg);
            b += u32::from(pb);
            n += 1;
        }
    }
    (n > 0).then(|| Rgb::new((r / n) as u8, (g / n) as u8, (b / n) as u8))
}

/// Finds the installed app behind a media session's AppUserModelID.
/// `apps` yields (AppsFolder ID, display name). Tries, in order: the exact
/// ID (Store apps like Spotify from the Store), the program file (Win32
/// players report e.g. "Spotify.exe"), then a display name that starts with
/// the program's name.
pub fn match_source<'a>(
    apps: impl Iterator<Item = (&'a str, &'a str)> + Clone,
    aumid: &str,
) -> Option<usize> {
    let aumid = aumid.trim();
    if aumid.is_empty() {
        return None;
    }
    if let Some(i) = apps
        .clone()
        .position(|(id, _)| id.eq_ignore_ascii_case(aumid))
    {
        return Some(i);
    }
    let lower = aumid.to_lowercase();
    let file = lower
        .rsplit(['\\', '/'])
        .next()
        .unwrap_or(&lower)
        .to_string();
    if file.ends_with(".exe") {
        let suffix = format!("\\{file}");
        if let Some(i) = apps
            .clone()
            .position(|(id, _)| id.to_lowercase().ends_with(&suffix))
        {
            return Some(i);
        }
    }
    let stem = source_fallback_name(aumid).to_lowercase();
    if stem.len() < 3 {
        return None;
    }
    apps.clone()
        .position(|(_, name)| name.to_lowercase().starts_with(&stem))
}

/// A readable name when the app isn't in the app list: "Spotify.exe" →
/// "Spotify", "Company.App_hash!App" → "Company.App".
pub fn source_fallback_name(aumid: &str) -> String {
    let file = aumid.rsplit(['\\', '/']).next().unwrap_or(aumid);
    let file = file.split('!').next().unwrap_or(file);
    let file = file.split('_').next().unwrap_or(file);
    let file = file
        .strip_suffix(".exe")
        .or_else(|| file.strip_suffix(".EXE"))
        .unwrap_or(file);
    file.to_string()
}

// ---------------------------------------------------------------------------
// Event subscriptions
// ---------------------------------------------------------------------------

/// Posts a plain message (no pointers) to our window. Used from Windows'
/// worker threads, so it takes the handle as a number.
fn post(target: isize, msg: u32, wparam: usize) {
    // SAFETY: PostMessageW only queues the numbers; if the window is gone
    // it simply fails.
    let _ = unsafe { PostMessageW(Some(HWND(target as *mut _)), msg, WPARAM(wparam), LPARAM(0)) };
}

#[derive(Debug, Clone, Copy)]
enum Event {
    Playback,
    Properties,
    Timeline,
}

/// One event handler on one session. Dropping it removes the handler.
struct Subscription {
    session: Session,
    event: Event,
    token: i64,
}

impl Subscription {
    fn new(session: &Session, event: Event, target: isize) -> Result<Self> {
        let token = match event {
            Event::Playback => {
                session.PlaybackInfoChanged(&TypedEventHandler::new(move |_, _| {
                    post(target, WM_APP_MEDIA, CHANGED_PLAYBACK);
                    Ok(())
                }))?
            }
            Event::Properties => {
                session.MediaPropertiesChanged(&TypedEventHandler::new(move |_, _| {
                    post(target, WM_APP_MEDIA, CHANGED_PROPERTIES);
                    Ok(())
                }))?
            }
            Event::Timeline => {
                session.TimelinePropertiesChanged(&TypedEventHandler::new(move |_, _| {
                    post(target, WM_APP_MEDIA, CHANGED_TIMELINE);
                    Ok(())
                }))?
            }
        };
        Ok(Self {
            session: session.clone(),
            event,
            token,
        })
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let removed = match self.event {
            Event::Playback => self.session.RemovePlaybackInfoChanged(self.token),
            Event::Properties => self.session.RemoveMediaPropertiesChanged(self.token),
            Event::Timeline => self.session.RemoveTimelinePropertiesChanged(self.token),
        };
        if let Err(e) = removed {
            // The app that owned the session may already be gone.
            log_info!("removing a media {:?} handler: {e}", self.event);
        }
    }
}

/// The manager plus its two handlers (removed on drop).
struct ManagerSubs {
    manager: Manager,
    sessions_token: i64,
    current_token: i64,
}

impl Drop for ManagerSubs {
    fn drop(&mut self) {
        let _ = self.manager.RemoveSessionsChanged(self.sessions_token);
        let _ = self.manager.RemoveCurrentSessionChanged(self.current_token);
    }
}

// ---------------------------------------------------------------------------
// The media state on the UI thread
// ---------------------------------------------------------------------------

/// A finished title / artist / cover fetch, sent from a worker thread.
struct Fetched {
    id: u64,
    info: Info,
}

pub struct Media {
    hwnd: HWND,
    manager_tx: Sender<Manager>,
    manager_rx: Receiver<Manager>,
    info_tx: Sender<Fetched>,
    info_rx: Receiver<Fetched>,
    manager: RefCell<Option<ManagerSubs>>,
    /// Every session, and a playback handler on each.
    sessions: RefCell<Vec<Session>>,
    playback_subs: RefCell<Vec<Subscription>>,
    /// The session the card shows, and its handlers.
    shown: RefCell<Option<Session>>,
    properties_sub: RefCell<Option<Subscription>>,
    timeline_sub: RefCell<Option<Subscription>>,
    /// Chosen with "Switch source"; cleared when Windows' current session
    /// changes.
    picked: RefCell<Option<Session>>,
    open: Cell<bool>,
    /// Title / artist / cover of the shown session.
    info: RefCell<Rc<Info>>,
    /// Id of the newest wanted fetch, and of the one `info` came from.
    wanted: Cell<u64>,
    have: Cell<u64>,
    fetching: Cell<bool>,
    /// Covers are decoded to this many pixels (44 DIPs at the DPI scale).
    cover_px: Cell<u32>,
}

impl Media {
    /// Asks Windows for the session manager. It arrives a moment later as
    /// [`WM_APP_MEDIA_READY`] (see [`Media::on_ready`]).
    pub fn new(hwnd: HWND, cover_px: u32) -> Self {
        let (manager_tx, manager_rx) = channel();
        let (info_tx, info_rx) = channel();
        let media = Self {
            hwnd,
            manager_tx,
            manager_rx,
            info_tx,
            info_rx,
            manager: RefCell::new(None),
            sessions: RefCell::new(Vec::new()),
            playback_subs: RefCell::new(Vec::new()),
            shown: RefCell::new(None),
            properties_sub: RefCell::new(None),
            timeline_sub: RefCell::new(None),
            picked: RefCell::new(None),
            open: Cell::new(false),
            info: RefCell::new(Rc::new(Info::default())),
            wanted: Cell::new(0),
            have: Cell::new(0),
            fetching: Cell::new(false),
            cover_px: Cell::new(cover_px.max(1)),
        };
        media.request_manager();
        media
    }

    fn target(&self) -> isize {
        self.hwnd.0 as isize
    }

    fn request_manager(&self) {
        let tx = self.manager_tx.clone();
        let target = self.target();
        let asked = Manager::RequestAsync().and_then(|op| {
            op.when(move |result| match result {
                Ok(manager) => {
                    if tx.send(manager).is_ok() {
                        post(target, WM_APP_MEDIA_READY, 0);
                    }
                }
                Err(e) => log_error!("media sessions unavailable: {e}"),
            })
        });
        if let Err(e) = asked {
            log_error!("asking for the media sessions failed: {e}");
        }
    }

    /// [`WM_APP_MEDIA_READY`]: subscribe to the manager and read the
    /// sessions for the first time.
    pub fn on_ready(&self) {
        let Ok(manager) = self.manager_rx.try_recv() else {
            return;
        };
        let target = self.target();
        let subscribed = (|| -> Result<ManagerSubs> {
            let sessions_token =
                manager.SessionsChanged(&TypedEventHandler::new(move |_, _| {
                    post(target, WM_APP_MEDIA, CHANGED_SESSIONS);
                    Ok(())
                }))?;
            let current_token =
                match manager.CurrentSessionChanged(&TypedEventHandler::new(move |_, _| {
                    post(target, WM_APP_MEDIA, CHANGED_CURRENT);
                    Ok(())
                })) {
                    Ok(token) => token,
                    Err(e) => {
                        let _ = manager.RemoveSessionsChanged(sessions_token);
                        return Err(e);
                    }
                };
            Ok(ManagerSubs {
                manager: manager.clone(),
                sessions_token,
                current_token,
            })
        })();
        match subscribed {
            Ok(subs) => {
                *self.manager.borrow_mut() = Some(subs);
                log_info!("media sessions ready");
                self.on_changed(CHANGED_SESSIONS | CHANGED_CURRENT);
            }
            Err(e) => log_error!("subscribing to media sessions failed: {e}"),
        }
    }

    /// The menu opened or closed. The timeline handler only exists while
    /// it is open (so a hidden menu costs nothing while music plays).
    pub fn set_open(&self, open: bool) {
        self.open.set(open);
        self.subscribe_timeline();
    }

    fn subscribe_timeline(&self) {
        // Drop the old one first: a handler is never added twice.
        self.timeline_sub.borrow_mut().take();
        if !self.open.get() {
            return;
        }
        if let Some(session) = self.shown.borrow().as_ref() {
            match Subscription::new(session, Event::Timeline, self.target()) {
                Ok(sub) => *self.timeline_sub.borrow_mut() = Some(sub),
                Err(e) => log_warn!("timeline events unavailable: {e}"),
            }
        }
    }

    /// Covers are decoded for this pixel size (changes with the DPI).
    pub fn set_cover_px(&self, px: u32) {
        let px = px.max(1);
        if self.cover_px.replace(px) != px && self.shown.borrow().is_some() {
            self.fetch();
        }
    }

    /// [`WM_APP_MEDIA`]: something changed (`bits` says what).
    pub fn on_changed(&self, bits: usize) {
        if self.manager.borrow().is_none() {
            return;
        }
        if bits & CHANGED_SESSIONS != 0 {
            self.reload_sessions();
        }
        if bits & CHANGED_CURRENT != 0 {
            self.picked.borrow_mut().take();
        }
        let shown_changed = self.choose();
        if shown_changed || bits & CHANGED_PROPERTIES != 0 {
            self.fetch();
        }
    }

    /// Reads the session list again and puts a playback handler on each.
    /// (Rare: only when an app starts or stops having media.)
    fn reload_sessions(&self) {
        let list: Vec<Session> = self
            .manager
            .borrow()
            .as_ref()
            .and_then(|m| m.manager.GetSessions().ok())
            .map(|v| v.into_iter().collect())
            .unwrap_or_default();
        // Remove every old handler before adding new ones.
        self.playback_subs.borrow_mut().clear();
        let target = self.target();
        let subs = list
            .iter()
            .filter_map(|s| {
                Subscription::new(s, Event::Playback, target)
                    .inspect_err(|e| log_warn!("playback events unavailable: {e}"))
                    .ok()
            })
            .collect();
        *self.playback_subs.borrow_mut() = subs;
        log_info!("media sessions: {}", list.len());
        *self.sessions.borrow_mut() = list;
    }

    fn states(&self) -> Vec<State> {
        self.sessions.borrow().iter().map(state_of).collect()
    }

    /// Where `session` is in the list: the same object, or else (Windows may
    /// hand out new wrappers) the same app.
    fn index_of(&self, session: &Session) -> Option<usize> {
        let sessions = self.sessions.borrow();
        sessions.iter().position(|s| s == session).or_else(|| {
            let id = aumid_of(session);
            sessions.iter().position(|s| aumid_of(s) == id)
        })
    }

    /// Picks the session to show. Returns true if it changed.
    fn choose(&self) -> bool {
        let states = self.states();
        let current = self
            .manager
            .borrow()
            .as_ref()
            .and_then(|m| m.manager.GetCurrentSession().ok())
            .and_then(|s| self.index_of(&s));
        let picked = self.picked.borrow().as_ref().and_then(|s| self.index_of(s));
        let chosen =
            pick(&states, current, picked).and_then(|i| self.sessions.borrow().get(i).cloned());

        let same = match (&chosen, self.shown.borrow().as_ref()) {
            (Some(a), Some(b)) => a == b,
            (None, None) => true,
            _ => false,
        };
        if same {
            return false;
        }
        if let Some(s) = &chosen {
            log_info!("music card shows {} ({:?})", aumid_of(s), state_of(s));
        } else {
            log_info!("music card hidden (nothing playing or paused)");
        }
        self.properties_sub.borrow_mut().take();
        *self.shown.borrow_mut() = chosen;
        if let Some(session) = self.shown.borrow().as_ref() {
            match Subscription::new(session, Event::Properties, self.target()) {
                Ok(sub) => *self.properties_sub.borrow_mut() = Some(sub),
                Err(e) => log_warn!("media property events unavailable: {e}"),
            }
        }
        self.subscribe_timeline();
        // Don't show the old song's title for the new app.
        *self.info.borrow_mut() = Rc::new(Info::default());
        true
    }

    // -----------------------------------------------------------------------
    // Title, artist and cover (async)
    // -----------------------------------------------------------------------

    /// Asks for the shown session's title, artist and cover. Only one fetch
    /// runs at a time; changes that come meanwhile start one more when it
    /// finishes (players often send several changes at once).
    fn fetch(&self) {
        self.wanted.set(self.wanted.get() + 1);
        if !self.fetching.get() {
            self.start_fetch();
        }
    }

    fn start_fetch(&self) {
        let Some(session) = self.shown.borrow().clone() else {
            return;
        };
        let id = self.wanted.get();
        let px = self.cover_px.get();
        let tx = self.info_tx.clone();
        let target = self.target();
        let started = session.TryGetMediaPropertiesAsync().and_then(|op| {
            op.when(move |result| {
                // This runs on a worker thread (or right here, if Windows
                // already had the answer): only plain data leaves it.
                let send = move |info: Info| {
                    if tx.send(Fetched { id, info }).is_ok() {
                        post(target, WM_APP_MEDIA_INFO, 0);
                    }
                };
                let props = match result {
                    Ok(props) => props,
                    Err(e) => {
                        log_warn!("reading the media properties failed: {e}");
                        send(Info::default());
                        return;
                    }
                };
                let text = |s: Result<windows::core::HSTRING>| {
                    s.map(|s| s.to_string_lossy().trim().to_string())
                        .unwrap_or_default()
                };
                let title = text(props.Title());
                let mut artist = text(props.Artist());
                if artist.is_empty() {
                    artist = text(props.AlbumArtist());
                }
                let info = Info {
                    title,
                    artist,
                    cover: None,
                };
                let opened = props.Thumbnail().and_then(|t| t.OpenReadAsync());
                let op = match opened {
                    Ok(op) => op,
                    Err(_) => return send(info), // no cover
                };
                let waited = op.when(move |stream| {
                    let mut info = info;
                    let decoded = stream.and_then(|s| decode_cover(&s, px));
                    match decoded {
                        Ok(cover) => info.cover = Some(cover),
                        Err(e) => log_warn!("decoding the album cover failed: {e}"),
                    }
                    send(info);
                });
                if let Err(e) = waited {
                    log_warn!("opening the album cover failed: {e}");
                }
            })
        });
        match started {
            Ok(()) => self.fetching.set(true),
            Err(e) => {
                log_warn!("asking for the media properties failed: {e}");
                self.have.set(id);
            }
        }
    }

    /// [`WM_APP_MEDIA_INFO`]: a fetch finished. Returns true if the shown
    /// info changed.
    pub fn on_info(&self) -> bool {
        let mut changed = false;
        while let Ok(fetched) = self.info_rx.try_recv() {
            self.fetching.set(false);
            // Older answers (the song changed meanwhile) are dropped.
            if fetched.id == self.wanted.get() {
                // What arrived, without the (personal) song names.
                let i = &fetched.info;
                log_info!(
                    "media info: title {}, artist {}, cover {}",
                    if i.title.is_empty() { "no" } else { "yes" },
                    if i.artist.is_empty() { "no" } else { "yes" },
                    i.cover.as_ref().map_or("none".to_string(), |c| format!(
                        "{} px, color {}",
                        c.px,
                        if c.color.is_some() { "yes" } else { "gray" }
                    )),
                );
                *self.info.borrow_mut() = Rc::new(fetched.info);
                self.have.set(fetched.id);
                changed = true;
            }
        }
        if !self.fetching.get() && self.have.get() != self.wanted.get() {
            self.start_fetch();
        }
        changed
    }

    // -----------------------------------------------------------------------
    // What the card shows, and its buttons
    // -----------------------------------------------------------------------

    /// The card's content right now, or `None` to hide it.
    pub fn view(&self) -> Option<MusicView> {
        let session = self.shown.borrow().clone()?;
        let playback = session.GetPlaybackInfo().ok()?;
        let playing = match playback.PlaybackStatus().ok()? {
            Status::Playing => true,
            Status::Paused => false,
            _ => return None,
        };
        let controls = playback.Controls().ok();
        let flag = |f: fn(&Controls) -> Result<bool>| {
            controls.as_ref().and_then(|c| f(c).ok()).unwrap_or(false)
        };
        let can_play_pause = flag(Controls::IsPlayPauseToggleEnabled)
            || if playing {
                flag(Controls::IsPauseEnabled)
            } else {
                flag(Controls::IsPlayEnabled)
            };
        let rate = playback
            .PlaybackRate()
            .and_then(|r| r.Value())
            .ok()
            .filter(|r| r.is_finite() && *r > 0.0)
            .unwrap_or(1.0);

        let (position, length) = session
            .GetTimelineProperties()
            .and_then(|t| {
                let start = t.StartTime()?.Duration;
                let length = (t.EndTime()?.Duration - start).max(0);
                let position = t.Position()?.Duration - start;
                let updated = t.LastUpdatedTime()?.UniversalTime;
                Ok((
                    current_position(position, updated, file_time_now(), playing, rate, length),
                    length,
                ))
            })
            .unwrap_or((0, 0));

        Some(MusicView {
            info: self.info.borrow().clone(),
            playing,
            can_previous: flag(Controls::IsPreviousEnabled),
            can_play_pause,
            can_next: flag(Controls::IsNextEnabled),
            can_seek: flag(Controls::IsPlaybackPositionEnabled) && length > 0,
            position,
            length,
            rate,
            at: Instant::now(),
            sources: self.states().iter().filter(|s| s.active()).count(),
            source_id: aumid_of(&session),
        })
    }

    /// Runs a button press. The answer comes back as events (the playback
    /// state changes), so the result here is only logged.
    pub fn command(&self, command: MediaCommand) {
        if command == MediaCommand::SwitchSource {
            self.switch_source();
            return;
        }
        let Some(session) = self.shown.borrow().clone() else {
            return;
        };
        let op = match command {
            MediaCommand::Previous => session.TrySkipPreviousAsync(),
            MediaCommand::PlayPause => session.TryTogglePlayPauseAsync(),
            MediaCommand::Next => session.TrySkipNextAsync(),
            MediaCommand::Seek(ticks) => {
                // The app counts from its own start time.
                let start = session
                    .GetTimelineProperties()
                    .and_then(|t| t.StartTime())
                    .map(|t| t.Duration)
                    .unwrap_or(0);
                session.TryChangePlaybackPositionAsync(start + ticks.max(0))
            }
            MediaCommand::SwitchSource => return,
        };
        let sent = op.and_then(|op| {
            op.when(move |result| match result {
                Ok(true) => {}
                Ok(false) => log_warn!("the player refused {command:?}"),
                Err(e) => log_warn!("{command:?} failed: {e}"),
            })
        });
        if let Err(e) = sent {
            log_warn!("{command:?} failed: {e}");
        }
    }

    fn switch_source(&self) {
        let states = self.states();
        let Some(shown) = self.shown.borrow().as_ref().and_then(|s| self.index_of(s)) else {
            return;
        };
        let Some(next) = next_source(&states, shown) else {
            return;
        };
        let session = self.sessions.borrow().get(next).cloned();
        *self.picked.borrow_mut() = session;
        if self.choose() {
            self.fetch();
        }
    }
}

fn state_of(session: &Session) -> State {
    match session.GetPlaybackInfo().and_then(|p| p.PlaybackStatus()) {
        Ok(Status::Playing) => State::Playing,
        Ok(Status::Paused) => State::Paused,
        _ => State::Other,
    }
}

fn aumid_of(session: &Session) -> String {
    session
        .SourceAppUserModelId()
        .map(|s| s.to_string_lossy())
        .unwrap_or_default()
}

/// Now, as a UTC file time (100 ns ticks since 1601), the same clock as
/// the timeline's `LastUpdatedTime`.
fn file_time_now() -> i64 {
    // SAFETY: no arguments, no preconditions.
    let ft = unsafe { GetSystemTimePreciseAsFileTime() };
    ((u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)) as i64
}

// ---------------------------------------------------------------------------
// Decoding the cover (on a worker thread)
// ---------------------------------------------------------------------------

/// COM for the current worker thread, if it doesn't have it yet.
struct MtaGuard(bool);

impl MtaGuard {
    fn new() -> Self {
        // SAFETY: initializes COM for this thread; undone in Drop only if
        // it succeeded (it fails harmlessly if the thread already chose a
        // different mode).
        Self(unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok())
    }
}

impl Drop for MtaGuard {
    fn drop(&mut self) {
        if self.0 {
            // SAFETY: pairs with the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
    }
}

/// Covers bigger than this are skipped (normal ones are 20–200 KB).
const MAX_COVER_BYTES: u64 = 8 * 1024 * 1024;

/// Decodes the cover straight to `px` × `px` (square covers; others are
/// cut to their middle square, e.g. 16:9 video thumbnails), plus its main
/// color from a 16 × 16 copy.
fn decode_cover(stream: &IRandomAccessStreamWithContentType, px: u32) -> Result<Cover> {
    let _com = MtaGuard::new();
    // SAFETY: plain COM calls on live interfaces; buffers are sized for
    // exactly what CopyPixels writes.
    unsafe {
        // The cover often lives in the player's process: copy it here with
        // as few (cross-process) reads as possible, then decode from memory.
        let size = stream.Size()?;
        if size == 0 || size > MAX_COVER_BYTES {
            return Err(Error::empty());
        }
        let istream: IStream = CreateStreamOverRandomAccessStream(stream)?;
        let mut bytes = vec![0u8; size as usize];
        let mut filled = 0usize;
        while filled < bytes.len() {
            let mut read = 0u32;
            istream
                .Read(
                    bytes[filled..].as_mut_ptr().cast(),
                    (bytes.len() - filled) as u32,
                    Some(&mut read),
                )
                .ok()?;
            if read == 0 {
                break;
            }
            filled += read as usize;
        }
        bytes.truncate(filled);
        let wic: IWICImagingFactory =
            CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
        let memory = wic.CreateStream()?;
        memory.InitializeFromMemory(&bytes)?;
        let decoder =
            wic.CreateDecoderFromStream(&memory, std::ptr::null(), WICDecodeMetadataCacheOnDemand)?;
        let frame = decoder.GetFrame(0)?;
        let (mut w, mut h) = (0u32, 0u32);
        frame.GetSize(&mut w, &mut h)?;
        let side = w.min(h);
        if side == 0 {
            return Err(Error::empty());
        }
        // Square covers (most music) go straight to the scaler, so a JPEG
        // can be decoded at a fraction of its size (much faster).
        let source: IWICBitmapSource = if w == h {
            frame.cast()?
        } else {
            let clipper = wic.CreateBitmapClipper()?;
            let square = WICRect {
                X: ((w - side) / 2) as i32,
                Y: ((h - side) / 2) as i32,
                Width: side as i32,
                Height: side as i32,
            };
            clipper.Initialize(&frame, &square)?;
            clipper.cast()?
        };
        let pixels = scaled(&wic, &source, px, &GUID_WICPixelFormat32bppPBGRA)?;
        // The color sample comes from the small copy we already have (the
        // file is decoded only once).
        let decoded =
            wic.CreateBitmapFromMemory(px, px, &GUID_WICPixelFormat32bppPBGRA, px * 4, &pixels)?;
        let small = scaled(&wic, &decoded.cast()?, 16, &GUID_WICPixelFormat32bppBGRA)?;
        Ok(Cover {
            px,
            pixels,
            color: dominant_color(&small),
        })
    }
}

/// `source` scaled to `px` × `px` pixels in `format` (32 bits per pixel).
fn scaled(
    wic: &IWICImagingFactory,
    source: &IWICBitmapSource,
    px: u32,
    format: &GUID,
) -> Result<Vec<u8>> {
    // SAFETY: plain COM calls; the buffer holds exactly px rows of px*4
    // bytes, which is what CopyPixels writes for this stride.
    unsafe {
        let scaler = wic.CreateBitmapScaler()?;
        scaler.Initialize(source, px, px, WICBitmapInterpolationModeFant)?;
        let converter = wic.CreateFormatConverter()?;
        converter.Initialize(
            &scaler,
            format,
            WICBitmapDitherTypeNone,
            None,
            0.0,
            WICBitmapPaletteTypeMedianCut,
        )?;
        let mut pixels = vec![0u8; (px * px * 4) as usize];
        converter.CopyPixels(std::ptr::null(), px * 4, &mut pixels)?;
        Ok(pixels)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P: State = State::Playing;
    const Z: State = State::Paused;
    const O: State = State::Other;

    #[test]
    fn picks_the_users_choice_then_current_then_playing() {
        // The user's pick wins while it is active.
        assert_eq!(pick(&[P, Z, P], Some(0), Some(1)), Some(1));
        // A stopped pick falls back to Windows' current session.
        assert_eq!(pick(&[P, O, P], Some(2), Some(1)), Some(2));
        // A stopped current session falls back to the first playing one.
        assert_eq!(pick(&[Z, O, P], Some(1), None), Some(2));
        // Nothing playing: the first paused one.
        assert_eq!(pick(&[O, Z, Z], None, None), Some(1));
        // Nothing active: hide the card.
        assert_eq!(pick(&[O, O], Some(0), None), None);
        assert_eq!(pick(&[], None, None), None);
        // Indexes out of range are ignored.
        assert_eq!(pick(&[P], Some(5), Some(9)), Some(0));
    }

    #[test]
    fn switch_source_cycles_through_active_sessions() {
        assert_eq!(next_source(&[P, O, Z, P], 0), Some(2));
        assert_eq!(next_source(&[P, O, Z, P], 2), Some(3));
        assert_eq!(next_source(&[P, O, Z, P], 3), Some(0));
        assert_eq!(next_source(&[P, O], 0), None);
        assert_eq!(next_source(&[], 0), None);
    }

    #[test]
    fn position_moves_only_while_playing() {
        let s = TICKS_PER_SECOND;
        // Reported at 10 s, 3 s ago, playing: 13 s.
        assert_eq!(
            current_position(10 * s, 100 * s, 103 * s, true, 1.0, 200 * s),
            13 * s
        );
        // Paused: stays at 10 s.
        assert_eq!(
            current_position(10 * s, 100 * s, 103 * s, false, 1.0, 200 * s),
            10 * s
        );
        // Double speed.
        assert_eq!(
            current_position(10 * s, 100 * s, 103 * s, true, 2.0, 200 * s),
            16 * s
        );
        // Never past the end, never below 0, unknown update time = as is.
        assert_eq!(
            current_position(190 * s, 100 * s, 150 * s, true, 1.0, 200 * s),
            200 * s
        );
        assert_eq!(current_position(-5, 0, 0, true, 1.0, 200 * s), 0);
        assert_eq!(current_position(7 * s, 0, 99 * s, true, 1.0, 0), 7 * s);
    }

    #[test]
    fn times_read_like_a_player() {
        let s = TICKS_PER_SECOND;
        assert_eq!(format_time(0), "0:00");
        assert_eq!(format_time(7 * s + 999), "0:07");
        assert_eq!(format_time(205 * s), "3:25");
        assert_eq!(format_time(3723 * s), "1:02:03");
        assert_eq!(format_time(-s), "0:00");
    }

    #[test]
    fn dominant_color_ignores_gray_and_transparent_pixels() {
        // BGRA: pure red, pure blue, gray, transparent green.
        let px = [
            0, 0, 200, 255, // red
            200, 0, 0, 255, // blue
            128, 128, 128, 255, // gray: ignored
            0, 255, 0, 0, // transparent: ignored
        ];
        assert_eq!(dominant_color(&px), Some(Rgb::new(100, 0, 100)));
        assert_eq!(dominant_color(&[90, 90, 90, 255, 0, 0, 0, 255]), None);
        assert_eq!(dominant_color(&[]), None);
    }

    #[test]
    fn finds_the_app_behind_a_session() {
        let apps = [
            (
                "Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic",
                "Media Player",
            ),
            (
                "{6D809377-6AF0-444B-8957-A3773F02200E}\\Spotify\\Spotify.exe",
                "Spotify",
            ),
            ("Opera.Browser.1234", "Opera Browser"),
        ];
        let it = apps.iter().map(|&(id, name)| (id, name));
        assert_eq!(
            match_source(
                it.clone(),
                "microsoft.zunemusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"
            ),
            Some(0)
        );
        assert_eq!(match_source(it.clone(), "Spotify.exe"), Some(1));
        assert_eq!(match_source(it.clone(), "opera.exe"), Some(2));
        assert_eq!(match_source(it.clone(), "Unknown.App_x!App"), None);
        assert_eq!(match_source(it, ""), None);
    }

    #[test]
    fn readable_names_for_unknown_apps() {
        assert_eq!(source_fallback_name("Spotify.exe"), "Spotify");
        assert_eq!(source_fallback_name("C:\\Apps\\vlc.exe"), "vlc");
        assert_eq!(source_fallback_name("Company.App_8abc!App"), "Company.App");
    }
}
