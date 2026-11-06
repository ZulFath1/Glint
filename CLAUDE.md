# Glint: a tiny, smooth Start menu for Windows 11

> **Glint**: a quick flash of light, like the snappy open animation.
> The binary is `glint.exe`; the crate and repo are named `glint`.

## 1. What this is (read first)

A replacement Start menu for Windows 11 that is **small, fast, smooth and
personal**. The owner is a student who wants something that feels nicer than
the stock Start menu: the stock one is too big, cluttered, can't be
personalized, and its animation feels stiff. Seelen UI was the inspiration,
but Seelen runs on WebView2 (a browser engine) and uses a lot of RAM.
**This project must stay native and light.**

The menu opens when the user presses the Win key. It **grows out of the
taskbar from 0 px** with an After Effects–style eased animation, and it is only
as big as its content needs.

Top to bottom, the menu has:

1. **Search bar.** Type to filter apps instantly.
2. **Pinned apps grid.**
3. **Music player.** Only visible when some app is playing or paused media.
4. **Footer.** Profile (avatar + name) on the left, power button on the right.

There is no "Recommended", no "Recent files", no category folders, no ads and
no web results.

The background can be **solid, acrylic (frosted glass), gradient (up to 3
colors), or acrylic + gradient**.

The visual reference (layout + animation + backgrounds + music player) is
the Design canvas "Simple Start Menu — Design". **The numbers in this file are
the source of truth.** If the mockup and this file disagree, this file wins.

The owner is still learning. After each milestone, explain in short, plain
words what you built and how to test it.

## 2. Hard goals (non-negotiable)

| Goal | Target |
|---|---|
| Idle RAM (menu hidden) | **< 25 MB** private working set (hard ceiling 35 MB) |
| RAM while open | **< 45 MB** (with acrylic + music player visible) |
| Idle CPU | **0%**: no timers or polling while hidden |
| Key press → first frame | **< 16 ms** (the window already exists, just hidden) |
| Animation frame rate | **The monitor's refresh rate** (60/120/144/165 Hz), run by the system compositor (DWM), never by our own frame loop |
| Memory leaks | **None**: RAM after 1,000 open/close cycles within ±1 MB of RAM after 10 cycles |

If a design choice conflicts with these, the goals win. If a target turns out
impossible, stop and report the measured numbers instead of quietly missing it.

## 3. Tech stack

- **Language:** Rust (stable), a single `.exe`, no installer.
- **Windows bindings:** the official `windows` crate (windows-rs), with only
  the features we use enabled (to keep compile time and binary size down).
- **UI layer: Windows.UI.Composition (the "Visual Layer"), hosted in a plain
  Win32 window.** No XAML, WinUI, WebView, Electron, Tauri or .NET.
  - Create a `DispatcherQueueController` on the UI thread
    (`CreateDispatcherQueueController`), then a `Compositor`, then a
    `DesktopWindowTarget` via `ICompositorDesktopInterop::CreateDesktopWindowTarget`.
  - Why the Visual Layer instead of plain DirectComposition:
    1. `KeyFrameAnimation` + `CubicBezierEasingFunction` give **exactly** the
       cubic-bezier curves below, played by DWM at the monitor's Hz.
    2. `Compositor::CreateHostBackdropBrush()` gives a **real blur of what is
       behind the window** that can be clipped to our rounded shape and scaled
       with the menu. That is acrylic, which DWM's window-level backdrop can't
       do during a scale animation.
    3. `CompositionLinearGradientBrush` gives gradients with animatable stops.
    4. `DropShadow`, `CompositionRoundedRectangleGeometry` clips, and
       `CompositionGeometricClip` are built in.
- **Drawing text and icons:** Direct2D + DirectWrite into
  `CompositionDrawingSurface`s, via `ICompositionGraphicsDeviceInterop` and
  `ICompositionDrawingSurfaceInterop` (one D3D11 device, one D2D device for
  the whole app).
- **Other crates (keep minimal):** `serde` + `toml` for config. A small
  hand-written fuzzy matcher, or `nucleo-matcher`. **Ask before adding any
  other dependency.**

## 4. Animation spec

All motion is composition animations. **No per-frame CPU drawing for motion,
and no `SetTimer` animation loops.**

**Open**
- Menu root visual: `Scale` goes `0 → 1` with `CenterPoint` at the **bottom
  center** (right above the taskbar Start button).
- Duration **340 ms** (configurable).
- Easing `cubic-bezier(0.2, 1.25, 0.35, 1)`: fast start, slight overshoot
  (~3–5%), soft settle. This is the After Effects "easy ease + overshoot" feel.
- `Opacity` `0 → 1` over the first 110 ms, linear.
- **Icon stagger:** each pinned icon visual fades in, rises 10 px, and scales
  `0.85 → 1`. Duration 300 ms, same overshoot easing. The first icon starts at
  70 ms, each next one starts **18 ms** later (configurable; use
  `DelayTime`).
- **Music card** pops in the same way, **40 ms after the last icon**.
- The search box gets keyboard focus on frame 1 (not after the animation), so
  typing works right away.

**Close**
- `Scale` `1 → 0`, **170 ms**, ease-in `cubic-bezier(0.4, 0, 1, 1)`, no
  overshoot. Opacity fades over the last 130 ms.
- When the animation finishes (`CompositionScopedBatch::Completed`), hide the
  window. **Do not destroy it.**

**Interrupting:** if the user reopens during the close (or the other way
round), start the new animation from the current values (use
`this.StartingValue` in the keyframes). Never jump.

**Layout changes while open** (music card appears or disappears, search
results change the row count): animate the size and positions over 220 ms
with `cubic-bezier(0.2, 0, 0, 1)`. Use implicit animations
(`ImplicitAnimationCollection`) on `Offset` and `Size`.

**Reduced motion:** if Windows "Animation effects" is off
(`SPI_GETCLIENTAREAANIMATION`, or `UISettings.AnimationsEnabled`), skip the
scale and stagger and only fade, 120 ms.

## 5. Layout spec

All sizes in DIPs. Use per-monitor DPI awareness v2 and scale correctly at
100/125/150/175/200%. Font: Segoe UI Variable (via DirectWrite).

**Menu shell**
- Corner radius **18**, 1 px border `rgba(255,255,255,0.10)`, drop shadow
  (blur 60, offset y 24, black 45%).
- Background: see section 6.

**Search bar**
- 40 px tall, radius 10, padding 16 (top/sides), 8 below.
- Fill `rgba(0,0,0,0.35)`, border `rgba(255,255,255,0.14)`, magnifier icon,
  placeholder "Search". Focused border = accent color.

**Pinned grid**
- Cells 80 × 80, gap 4, icon 38 px, label 12 px under it, single line with
  ellipsis.
- Hover: `rgba(255,255,255,0.08)` with radius 12. Keyboard focus: a 2 px accent
  ring.
- **Columns** = `min(4, pinned_count)`. **Rows** = as many as needed.
- Menu width = `max(320, columns × 84 + 24)`. **The menu size follows the
  content.** Fewer pins = smaller menu.

**Music card** (section 8)
- Margin 12 on the sides, 12 below. Radius 14, fill `rgba(0,0,0,0.28)`, border
  `rgba(255,255,255,0.08)`, padding 10/12.
- Row 1: cover 44 × 44 (radius 8); title (13 px, semibold) and artist (12 px)
  stacked with ellipsis; buttons Previous / Play-Pause / Next (hit targets at
  least 36 × 44).
- Row 2: elapsed time, seek bar (4 px track, 12 px thumb), total time.
- Row 3: "Playing from <app name>" and a "Switch source" pill, shown only
  when there are 2+ media sessions.

**Footer**
- 1 px top divider, fill `rgba(0,0,0,0.22)`.
- Left: avatar (30 px circle) + display name. Right: power button (44 × 44).

**Text colors:** primary `#f4f4f5`, secondary `#d4d4d8`. Text must keep
**4.5:1 contrast** against whatever background is under it (see section 6).

**Position:** open on the monitor with the taskbar the user interacted with
(or the cursor's monitor), centered above the taskbar's Start button area.
Handle a taskbar at the bottom, top, left or right; the scale origin is the
edge nearest the taskbar. Gap between menu and taskbar: 12 px.

## 6. Backgrounds (personalization)

Four modes:

| Mode | What it looks like | How to build it |
|---|---|---|
| `solid` | One flat color | `CompositionColorBrush` |
| `acrylic` | Frosted glass: blurred wallpaper/windows behind, tinted | `HostBackdropBrush` visual, with a tint visual on top (`colors[0]` at 55% alpha) and an optional 2% noise texture |
| `gradient` | 2–3 colors blending at an angle | `CompositionLinearGradientBrush`, stops evenly spaced, rotated by `angle` |
| `acrylic-gradient` | Glass tinted with a gradient (the default) | `HostBackdropBrush` + the gradient brush at 55% alpha on top |

Stack these layers inside the menu's rounded clip, bottom to top:
backdrop → tint/gradient → **overlay** (black at `overlay` alpha, 0–0.6) →
content.

**Readability guard:** after choosing colors, compute the contrast of
`#f4f4f5` text against each gradient stop *after* the overlay. If any stop is
below 4.5:1, raise the effective overlay in 0.05 steps until it passes. Never
lower the user's overlay. Log when this happens.

**Fallbacks.** Acrylic falls back to the same tint as a solid color (no blur)
when:
- Windows "Transparency effects" is off (`UISettings.AdvancedEffectsEnabled`),
- battery saver is on, or
- `HostBackdropBrush` creation fails.

Re-check whenever these settings change.

High contrast mode: ignore the theme and use system colors
(`GetSysColor(COLOR_WINDOW)` / `COLOR_WINDOWTEXT`).

**Presets** (used when `preset != "custom"`):

| Preset | Colors |
|---|---|
| `midnight` | `#1e1b3a`, `#14213d` |
| `aurora` | `#0f3b3a`, `#1c2350`, `#3a1c4a` (default) |
| `sunset` | `#4a1d3f`, `#3d2140`, `#1f1b3a` |
| `ocean` | `#0b2a3d`, `#0e3b4a` |
| `plain` | `#1e2026` |

**Album tint (optional, off by default):** when `tint_from_album = true` and
music is showing, take the cover's dominant color and use it as a darkened
replacement for the **first** gradient stop. To get the color: downscale the
cover to 16×16 with WIC, average the pixels with saturation > 0.25, then darken
until the contrast rule passes. Cross-fade the stop color over 400 ms with a
`ColorKeyFrameAnimation`.

**No animated or moving gradients**: they cost GPU/CPU the whole time the
menu is open.

**Performance:** solid and gradient are free (brushes, nothing redrawn).
Measure the RAM and GPU cost of acrylic in milestone 2 and report it.

## 7. Search box (we draw it ourselves, so do it properly)

There is no XAML TextBox, so implement a small but correct single-line editor:

- Text input via `WM_CHAR` (handle UTF-16 surrogate pairs).
- Keys:
  - Backspace, Delete, Ctrl+Backspace (delete word)
  - Left/Right, Ctrl+Left/Right (word jump)
  - Home/End, Shift+arrows to select, Ctrl+A
  - Ctrl+C, Ctrl+V (strip newlines), Ctrl+X
  - Esc: clear the text if there is any, otherwise close the menu
- **IME** (Chinese/Japanese/Korean/Vietnamese input): handle
  `WM_IME_STARTCOMPOSITION` / `WM_IME_COMPOSITION` / `WM_IME_ENDCOMPOSITION`
  with IMM32. Show the composition string underlined at the caret, insert
  `GCS_RESULTSTR`, and position the candidate window with
  `ImmSetCandidateWindow`.
- **Caret:** a 1.5 px accent-colored visual. Make it blink with a looping
  composition opacity animation (530 ms on/off), **not** a timer. Restart the
  blink (solid) on each keystroke.
- Text that overflows scrolls horizontally to keep the caret visible.
- Measure text with DirectWrite `IDWriteTextLayout` (hit testing for caret
  position and mouse clicks).

## 8. Music player (media controls)

Use the Windows media session API that powers the system media flyout:
`Windows.Media.Control.GlobalSystemMediaTransportControlsSessionManager`.
It works with Spotify, browsers (YouTube in Opera/Chrome/Edge), Media Player
and most other players.

- **Setup:** `RequestAsync()` once at startup. Subscribe to `SessionsChanged`
  and `CurrentSessionChanged`. For the shown session, subscribe to
  `MediaPropertiesChanged`, `PlaybackInfoChanged` and
  `TimelinePropertiesChanged`. **Event-driven only, no polling.**
- **Show / hide:** show the card only if a session exists whose
  `PlaybackStatus` is Playing or Paused. Otherwise hide it (the menu shrinks,
  with the 220 ms layout animation).
- **Data:** from `TryGetMediaPropertiesAsync()` take `Title`, `Artist` (fall
  back to `AlbumArtist`) and `Thumbnail`. Decode the thumbnail with WIC
  straight to **44 × 44 × DPI scale** and cache only the current cover.
- **Controls:** `TrySkipPreviousAsync`, `TryTogglePlayPauseAsync`,
  `TrySkipNextAsync`, and `TryChangePlaybackPositionAsync` (100 ns ticks).
  Disable (dim, no hover) each button whose
  `PlaybackInfo.Controls.Is…Enabled` is false.
- **Seek bar:** position = `Timeline.Position + (now − LastUpdatedTime)` while
  Playing. Update it **only while the menu is open**, at most 4 times per
  second (it only needs to move smoothly enough to read). Dragging seeks on
  release.
- **Switch source:** list the sessions. The pill cycles through them. The app
  name and icon come from `SourceAppUserModelId`, resolved through the same
  AppsFolder lookup as the app list. Default to `GetCurrentSession()`.
- **Keyboard:** when the search box is empty, Space = play/pause, and
  Ctrl+Left/Right = previous/next.
- All WinRT async calls complete on the dispatcher queue thread. Never block
  the UI thread on `.get()`.

## 9. Behavior

**Opening (the hardest part, test it well)**
- The **Win key alone** (press + release with no other key in between)
  toggles the menu.
- Implement it with a low-level keyboard hook (`WH_KEYBOARD_LL`):
  1. On LWin/RWin **down**: swallow it and remember "Win is held".
  2. If **another key** goes down while Win is held (Win+E, Win+D,
     Win+Shift+S, Win+arrows…): first inject a Win down with `SendInput`, then
     let the key through. From then on pass everything through normally,
     including the Win up.
  3. On Win **up** with no other key pressed: swallow it and toggle our menu.
  4. Mark our injected events (`dwExtraInfo` = a unique tag) and ignore them
     in the hook.
- Keep the hook callback tiny: post a message to the UI thread and return.
  Windows silently removes hooks that are slow (`LowLevelHooksTimeout`), so
  detect that case and reinstall the hook.
- **Foreground focus:** Windows may refuse `SetForegroundWindow` from a
  background process. Because we just received the user's key press, it is
  usually allowed. If it fails, use the known workaround (a synthetic
  Alt down/up via `SendInput` right before `SetForegroundWindow`) and verify
  the search box really has focus.
- Optional extra hotkey (e.g. `Alt+Space`) via `RegisterHotKey`.
- Clicking the real taskbar Start button is out of scope for v1. In the
  README, document how to hide it (e.g. with a Windhawk taskbar mod).

**Closing:** Esc (when search is empty), the Win key again, losing focus
(`WM_ACTIVATE` with `WA_INACTIVE`), or launching an app.

**Search results**
- Typing filters the pinned apps **and** all installed apps.
- When the query is not empty, replace the grid with a list of the top 8
  matches (icon 24 px + name, rows 40 px). The first result is highlighted,
  arrow keys move the highlight, and Enter launches it.
- Ranking order: exact prefix > word-start match (e.g. "ps" → Photoshop) >
  fuzzy subsequence. Ties go to pinned apps, then to recently launched apps
  (keep launch counts in the config).
- v1 searches apps only.

**App list**
- Enumerate `shell:AppsFolder` (`FOLDERID_AppsFolder`, `IShellItem` /
  `IEnumShellItems`). This covers Win32 *and* Store/UWP apps.
- Each item: display name + `PKEY_AppUserModel_ID` (AUMID). Win32 apps often
  have path-like IDs such as
  `{6D809377-6AF0-444B-8957-A3773F02200E}\...\app.exe`. That is normal.
- Launch with `ShellExecuteW` on `shell:AppsFolder\<AUMID>`.
- Icons via `IShellItemImageFactory::GetImage` at exactly the drawn pixel
  size. Convert to D2D bitmaps **once** and keep them in an LRU cache of at
  most 64. Free the `HBITMAP`s right away.
- Watch for installs/uninstalls with `SHChangeNotifyRegister`, debounced to
  2 s. No polling.

**Pinned apps:** stored as AUMIDs in the config. Right-click an app shows
"Pin / Unpin". If a pinned app was uninstalled, skip it silently.

**Profile:** the display name comes from `GetUserNameExW(NameDisplay)`,
falling back to the user name. The avatar is the user's account picture,
falling back to a generic person icon. Clicking it opens
`ms-settings:accounts`.

**Power button:** opens a small popup (same animation style, smaller)
with these items:

| Item | API |
|---|---|
| Lock | `LockWorkStation` |
| Sleep | `SetSuspendState(false, false, false)` |
| Restart | `InitiateShutdownW` |
| Shut down | `InitiateShutdownW` |
| Sign out | `ExitWindowsEx(EWX_LOGOFF)` |

Restart and Shut down ask "Are you sure?" inside the popup, not with a
system dialog.

**Window details:** create the window with
`WS_EX_NOREDIRECTIONBITMAP | WS_EX_TOOLWINDOW | WS_EX_TOPMOST` and
`WS_POPUP`, so it has no taskbar button and only composition draws in it.
Size the window to the menu bounds plus a 32 px shadow margin, and resize it
before opening when the content size changed.

## 10. Config

`%APPDATA%\Glint\config.toml` is created with defaults on first run.
Hot-reload it on change (`ReadDirectoryChangesW`, debounced), and keep the
last good config if the file has a syntax error (log the error).

```toml
pinned = [
  "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
  "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App",
  "Microsoft.Windows.Explorer",
  "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel",
  "Microsoft.Paint_8wekyb3d8bbwe!App",
  "Microsoft.Windows.Photos_8wekyb3d8bbwe!App",
]
accent = "#8fa8ff"             # or "system" to follow the Windows accent color
hotkey = ""                    # e.g. "Alt+Space"; empty = Win key only
max_columns = 4

[background]
mode = "acrylic-gradient"      # "solid" | "acrylic" | "gradient" | "acrylic-gradient"
preset = "aurora"              # "midnight" | "aurora" | "sunset" | "ocean" | "plain" | "custom"
colors = ["#2b1a4a", "#1a2347", "#0f2a3a"]   # used when preset = "custom"; 1–3 colors
angle = 135                    # gradient direction, degrees
overlay = 0.2                  # 0.0–0.6, darkens for readability
tint_from_album = false

[music]
enabled = true                 # false = never show the music card

[animation]
open_ms = 340
close_ms = 170
stagger_ms = 18
overshoot = 1.25               # 2nd control-point y of the open easing (1.0 = no bounce)

[stats]                        # written by the app, not by the user
launch_counts = {}
```

On first run, keep only the default pins that exist on this PC.

## 11. Memory & leak rules

- Create the window, compositor, devices, brushes and surfaces **once**.
  Opening and closing only toggles visibility and starts animations.
- COM/WinRT objects live in windows-rs smart pointers. No manual `AddRef`.
- Every GDI handle (`HBITMAP`, `HICON`, `HDC`) is released in the function that
  gets it, or wrapped in an RAII guard.
- Event subscriptions (media, settings) are stored with their tokens and
  removed when replaced. Re-subscribing without removing is the classic leak.
- Caches are bounded: 64 icons, 1 album cover.
- Device loss (`D2DERR_RECREATE_TARGET`, `CompositionGraphicsDevice`
  `RenderingDeviceReplaced`): recreate the devices and redraw the surfaces.
  Drop the old ones.
- After startup and after each close, call `SetProcessWorkingSetSize(-1, -1)`
  **only if measurement shows it helps**.
- Add a `--stress N` debug flag. It opens and closes the menu N times
  (including music-card show/hide and background mode switches) and logs the
  private working set before and after. **Check: 1,000 cycles, growth < 1 MB.**

## 12. Known hard parts (slow down here and test)

1. **The Win-key hook** (section 9). Test every common Win+ combo, holding
   Win for a long time, Win + mouse click, and fast double presses. A bug
   here breaks the user's whole PC.
2. **Focus stealing:** the search box must actually receive typing on the
   first key press after opening.
3. **The search editor and IME** (section 7).
4. **HostBackdropBrush** inside a scaled, rounded-clipped visual: check that it
   looks right *during* the scale animation, on multiple monitors, and at
   different DPIs.
5. **DPI changes** (`WM_DPICHANGED`, moving between monitors): redraw the
   surfaces at the new scale and re-fetch icons at the new pixel size.
6. **WinRT async on the UI thread:** never block on it. Everything goes
   through the dispatcher queue.

## 13. Project layout

```
glint/
  Cargo.toml
  src/
    main.rs            # single-instance mutex, dispatcher queue, message loop, tray icon
    hook.rs            # Win-key low-level hook + tests for combo pass-through
    window.rs          # popup window, DPI, positioning, focus
    ui/
      mod.rs           # Compositor, DesktopWindowTarget, D3D/D2D devices, device loss
      shell.rs         # menu root visual, clip, shadow, layout, implicit animations
      background.rs    # solid / acrylic / gradient / overlay, contrast guard, fallbacks
      anim.rs          # easing curves → KeyFrameAnimations, open/close/stagger
      search_box.rs    # text editor, caret, IME
      grid.rs          # pinned grid + search results list
      music.rs         # music card drawing
      footer.rs        # profile + power popup
    apps.rs            # AppsFolder enumeration, launching, change notify
    icons.rs           # icon extraction + LRU cache
    search.rs          # matcher + ranking
    media.rs           # GlobalSystemMediaTransportControls sessions + events
    power.rs           # lock/sleep/restart/shutdown/sign out
    config.rs          # TOML load/save/hot reload, presets
    settings_watch.rs  # transparency, battery saver, accent, high contrast, animation setting
  README.md
```

## 14. Milestones (in order; stop after each one so the owner can test)

1. **Skeleton:** tray icon (with Quit), single instance, hidden composition
   window, Win-key hook that toggles an empty rounded rectangle.
   **Measure and report the RAM.**
2. **Animation + background:** open/close with the exact curves,
   interruptible, positioned above the taskbar, per-monitor DPI. All 4
   background modes, presets, overlay, the contrast guard and fallbacks.
   **Report the RAM for each background mode.**
3. **Content:** search box (full editor + IME), pinned grid with real icons
   and stagger, footer with profile and power popup.
4. **Search:** all apps, ranking, results list, keyboard navigation, launching.
5. **Music player:** sessions, card show/hide with the layout animation,
   controls, seek bar, source switching, optional album tint.
6. **Config:** TOML, hot reload, pin/unpin from right-click, launch counts.
7. **Hardening:** the `--stress` leak test, device loss, multi-monitor,
   taskbar on other edges, reduced motion, high contrast, autostart (opt-in
   HKCU `Run` key), and a README with install and uninstall steps.

After every milestone, commit it (section 16) and report:
- idle RAM and open RAM,
- whether any target was missed,
- how to test what was just built (short, plain steps).

## 15. Conventions

- `cargo clippy -- -D warnings` and `cargo fmt` must pass.
- Keep `unsafe` blocks small, with a comment explaining why each one is sound.
- No `unwrap()` outside tests and startup. Log errors to
  `%LOCALAPPDATA%\Glint\log.txt` (rotating, max 1 MB).
- Release profile: `opt-level = 3`, `lto = true`, `codegen-units = 1`,
  `panic = "abort"`, `strip = true`.
- Target: Windows 11 22H2+, x64.
- Never change system settings or the registry outside our own keys.
  Never kill or patch `explorer.exe`.

## 16. Git & portfolio

This project will be published on GitHub as the owner's **portfolio piece**.
Keep the history clean and readable.

**Git workflow**
- The repo is initialized at the start (before milestone 1), with a Rust
  `.gitignore` that also ignores `target/`, `*.log`, `config.toml`,
  `.vscode/` and `.env`.
- **Commit after each milestone works** and the owner has tested it, with a
  message like `milestone 2: open/close animation + background modes`.
  Smaller commits inside a milestone are fine (`feat: win-key hook`,
  `fix: focus on first keypress`), using Conventional Commit style
  (`feat:`, `fix:`, `perf:`, `refactor:`, `docs:`, `test:`, `chore:`).
- **Never commit broken code to `main`.** If an experiment is risky, use a
  branch (`git switch -c try-acrylic`) and merge it when it works.
- Never commit secrets, personal paths, the owner's real config, or logs.
- Before any `git push`, show the owner what will be pushed (`git log
  origin/main..HEAD --oneline` or the full list for a first push) and ask.
- Publishing: use the GitHub CLI (`gh`). Install it with
  `winget install GitHub.cli` if missing, run `gh auth login` (the owner logs
  in through the browser), then `gh repo create glint --public --source . --push`.
  Ask the owner before creating the repo.

**README.md (write it in milestone 7; keep a draft updated as you go)**

Order of sections:
1. **Name + one-line pitch:** "Glint: a tiny, buttery-smooth Start menu for
   Windows 11, written in Rust."
2. **Demo GIF** at the top (`docs/demo.gif`): the menu opening and closing.
   Leave a placeholder and tell the owner how to record it (e.g. with ScreenToGif).
3. **Features:** short bullet list (grow animation, search, pinned apps,
   music player, 4 background modes + presets, config hot reload).
4. **Performance table:** Glint's idle RAM, open RAM, idle CPU and binary size
   next to the stock Windows Start menu (`StartMenuExperienceHost.exe`),
   measured on the owner's PC. Say how it was measured.
5. **Screenshots:** each background preset and the music player
   (`docs/screenshots/`).
6. **Install:** download `glint.exe` from Releases, run it, optional autostart.
   **Uninstall:** quit from the tray, delete the exe and `%APPDATA%\Glint`.
7. **Configuration:** the `config.toml` example with comments.
8. **How it's built:** Rust + windows-rs, Windows.UI.Composition for
   compositor-driven animations at the monitor's refresh rate, a low-level
   keyboard hook, the media session API, and why there's no WebView.
9. **Build from source:** `cargo build --release`.
10. **Roadmap**, **License (MIT)**, **Credits**.

**Also in milestone 7**
- Add `LICENSE` (MIT, with the owner's name; ask for it).
- Add a GitHub Actions workflow (`.github/workflows/ci.yml`) on
  `windows-latest` that runs `cargo fmt --check`, `cargo clippy -- -D warnings`,
  `cargo test` and `cargo build --release`.
- Prepare release `v0.1.0`: build the release exe and draft release notes.
  Create the release with `gh release create` and attach `glint.exe`, but
  only after the owner says go.

## 17. Out of scope for v1

- File and web search
- Widgets
- A custom taskbar
- Opening from a click on the real taskbar Start button
- Animated gradients
- Touch gestures
- A settings screen (the config file is the settings for now)
- Drag-to-reorder pins
