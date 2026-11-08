# Glint

**Glint: a tiny, buttery-smooth Start menu for Windows 11, written in Rust.**

Press the Win key and a small menu grows out of the taskbar, with a soft
bounce. It holds a search box, your pinned apps, a music player and a power
button, and nothing else: no ads, no web results, no "Recommended".

![Glint opening and closing](docs/demo.gif)

## Features

- **Grow animation.** The menu scales up from the taskbar with an eased,
  slightly bouncy curve and shrinks back when it closes. Pinned icons pop in
  one after another. Every animation is played by the Windows compositor at
  your monitor's refresh rate (60, 120, 144 Hz...). Closing halfway and
  reopening continues smoothly instead of jumping.
- **Only as big as it needs to be.** The grid has 6 columns by default (3 to
  8, your choice), and the menu shrinks or grows with the number of pins.
- **Instant app search.** Start typing to search all installed apps, both
  classic programs and Store apps. Exact prefixes rank first, then word
  starts ("ps" finds Photoshop), then looser matches. Apps you launch often
  move up. Arrow keys and Enter work.
- **A real text box.** Selection, word jumps, copy/paste, Ctrl+Backspace, and
  IME input for Chinese, Japanese, Korean and Vietnamese.
- **Pinned apps.** Right-click any app to pin or unpin it.
- **Music player.** It appears when Spotify, a browser tab (YouTube...), Media
  Player or another app is playing or paused. It shows the cover, title and
  artist, with previous / play-pause / next buttons, a seek bar, and a
  "Switch source" button when two players are active. It hides when nothing
  is playing.
- **5 themes, 3 layouts.** Glint, Catppuccin, Nord, Tokyo Night and Gruvbox,
  each with its own colors, shapes, font and layout (grid, list or sidebar).
  Switch in Settings → Theme; the menu changes live. See [Themes](#themes).
- **4 background modes + 5 color presets.** Solid, gradient, frosted glass,
  or glass tinted with a gradient (the default). Presets: midnight, aurora,
  sunset, ocean, plain, or your own 1 to 3 colors. If your colors are too
  bright for white text, Glint darkens them a little by itself so text stays
  readable. The background can also take its color from the album cover.
- **Power button.** Lock, Sleep, Restart and Shut down, with an "Are you
  sure?" step for Restart and Shut down.
- **Profile.** Your account picture and name; click to open account settings.
- **Settings window.** Change the background, colors, columns, animation,
  music player, hotkey and more, and watch the menu change next to it as you
  go. Open it from the tray icon or the gear button in the menu. It is built
  with the same native stack as the menu, and closing it gives its memory
  back.
- **Config file with hot reload.** Edit `config.toml`, save, and the menu
  changes right away. A typo doesn't break anything: Glint keeps the last
  good settings and tells you the line number.
- **Plays nice with Windows.** Win+E, Win+D, Win+Shift+S and every other Win
  shortcut still work. Glint follows the "Animation effects", "Transparency
  effects", battery saver, accent color and high contrast settings, and
  supports per-monitor DPI and a taskbar on any edge.
- **Optional "Start with Windows"** from the tray icon.

## Performance

| | Glint 0.1.0 | Windows Start menu |
|---|---|---|
| Memory (private working set), menu hidden | 5–14 MB | ~17 MB |
| Memory (private working set), menu open | 10–12 MB | ~17 MB |
| Working set including shared memory | 12–50 MB | ~120 MB |
| Private bytes (all memory the process has reserved) | 25–30 MB | ~75 MB |
| CPU while hidden | 0 ms over 60 s | 0 ms over 60 s |
| Size on disk | one 1.0 MB `.exe` | built into Windows |

**How this was measured:** on the author's PC (Windows 11, build 26300),
November 2026, with the release build and 6 pinned apps. "Private working
set" is the "Memory" column in Task Manager, read with
[`tools/measure-ram.ps1`](tools/measure-ram.ps1); the Windows Start menu is
the `StartMenuExperienceHost.exe` process. Glint's numbers are ranges
because Windows trims the memory of hidden apps, so the reading depends on
how long Glint has been idle (5.3 MB right after starting, about 13 MB after
a few hours of use). The "open" number was taken with Spotify playing, so the
music card was showing. CPU is the process's total CPU time over 60 seconds
with the menu closed. Glint uses no timers or polling while hidden.

**Leak test:** `glint.exe --stress 1000` opens and closes the menu 1,000
times while typing in search, switching backgrounds, showing and hiding the
music card, changing pins and reloading the config. Over the last 900 of
those cycles, handles and GDI objects stay flat and private bytes grow by
0.2 MB. The private working set grows by about 1.2 MB, a little over the
1 MB goal, so that is still being looked at (see
[Known limitations](#known-limitations)).

## Screenshots

| Aurora (default) | Midnight | Sunset |
|---|---|---|
| ![Aurora preset](docs/screenshots/aurora.png) | ![Midnight preset](docs/screenshots/midnight.png) | ![Sunset preset](docs/screenshots/sunset.png) |

| Ocean | Plain | Music player |
|---|---|---|
| ![Ocean preset](docs/screenshots/ocean.png) | ![Plain preset](docs/screenshots/plain.png) | ![Music player](docs/screenshots/music.png) |

| Search | Power menu |
|---|---|
| ![Search results](docs/screenshots/search.png) | ![Power menu](docs/screenshots/power.png) |

## Themes

A theme is a color palette, shapes (corners, borders), a font and a layout.
Pick one in **Settings → Theme**, or write `theme = "nord"` in `config.toml`.
The background and accent settings still work on top of a theme: picking a
theme sets them to `"theme"` (its own colors), and you can change them
afterwards.

| Theme | Layout | Look |
|---|---|---|
| **Glint** (default) | grid | Glass and soft gradients, the original look |
| **Catppuccin** (Mocha) | grid | Soft and cozy: pill search bar, mauve play button, separate Lock / Restart / Shut down buttons |
| **Nord** | list | Calm and minimal: a Rofi-style list of your most-used apps, an icon-only pinned strip, a one-line music bar |
| **Tokyo Night** | sidebar | Neon dashboard: a rail on the left, a greeting with a big clock, icons on colored tiles |
| **Gruvbox** (dark) | grid | Terminal style: monospace font, `user@glint: ~` title bar, `❯` prompt, `[ lock ] [ restart ] [ off ]` |

**Icon tint** (Settings → Theme, or `icon_tint = true`) draws every app icon
in one color, the theme's text color.

| Glint | Catppuccin | Nord |
|---|---|---|
| ![Glint theme](docs/screenshots/theme-glint.png) | ![Catppuccin theme](docs/screenshots/theme-catppuccin.png) | ![Nord theme](docs/screenshots/theme-nord.png) |

| Tokyo Night | Gruvbox |
|---|---|
| ![Tokyo Night theme](docs/screenshots/theme-tokyo-night.png) | ![Gruvbox theme](docs/screenshots/theme-gruvbox.png) |

## Install

Requirements: Windows 11 22H2 or newer, 64-bit (x64).

1. Download `glint.exe` from the
   [latest release](https://github.com/ZulFath1/Glint/releases/latest).
2. Put it in a folder that will stay, for example
   `%LOCALAPPDATA%\Programs\Glint\`.
3. Double-click it. Windows SmartScreen may warn you because the exe is not
   code-signed: click **More info**, then **Run anyway**.
4. Glint's icon appears in the system tray. Press the Win key.
5. Optional: right-click the tray icon and choose **Start with Windows**.

Clicking the real Start button on the taskbar still opens the Windows Start
menu. If you want, you can hide that button, for example with a
[Windhawk](https://windhawk.net/) taskbar mod.

### Using it

| Key / action | What it does |
|---|---|
| Win (press and release) | Open or close the menu |
| Type | Search apps |
| ↑ ↓ and Enter | Pick a search result and launch it |
| Esc | Clear the search, or close the menu if it's empty |
| Right-click an app | Pin or unpin it |
| Space (search empty) | Play / pause music |
| Ctrl+← / Ctrl+→ (search empty) | Previous / next track |
| Click outside the menu | Close it |
| Gear button (or tray icon > Settings…) | Open Settings |

### Uninstall

1. If you turned on **Start with Windows**, turn it off first (right-click
   the tray icon). That removes Glint's entry from the startup list.
2. Right-click the tray icon and choose **Quit Glint**.
3. Delete `glint.exe`.
4. Delete the `%APPDATA%\Glint` folder (settings) and the
   `%LOCALAPPDATA%\Glint` folder (log).

Glint does not change any other Windows setting, so nothing else needs to be
undone.

## Configuration

The easy way to change settings is the **Settings** window (tray icon >
**Settings…**, or the gear in the menu). Everything it changes is saved to
`%APPDATA%\Glint\config.toml`, which you can also edit by hand. Glint
creates it on first run (keeping only the default pins that exist on your
PC). Open it from the tray icon with **Open config file**. Save the file and
the menu (and an open Settings window) updates right away. Your comments in
the file are kept when Glint saves.

```toml
# The easy way to change these: right-click the Glint tray icon, then
# "Settings…". Or edit this file: Glint watches it, so when you save, the
# menu changes right away.

# Pinned apps, in grid order, as app IDs (AppUserModelIDs).
# The easy way to change them: right-click an app in the menu, then
# "Pin to Glint" or "Unpin from Glint". Apps that aren't installed are skipped.
pinned = [
  "Microsoft.WindowsCalculator_8wekyb3d8bbwe!App",
  "Microsoft.WindowsNotepad_8wekyb3d8bbwe!App",
  "Microsoft.Windows.Explorer",
  "windows.immersivecontrolpanel_cw5n1h2txyewy!microsoft.windows.immersivecontrolpanel",
  "Microsoft.Paint_8wekyb3d8bbwe!App",
  "Microsoft.Windows.Photos_8wekyb3d8bbwe!App",
]

# The look: "glint", "catppuccin", "nord", "tokyo-night" or "gruvbox".
# A theme brings its own colors, shapes and font; `accent = "theme"` and
# `preset = "theme"` below use the theme's colors.
theme = "glint"

# Draw app icons in one color (the theme's text color).
icon_tint = false

# Color of the text caret, keyboard focus ring, search border and seek bar:
# "theme", "#rrggbb", or "system" to follow your Windows accent color.
accent = "theme"

# An extra shortcut that opens the menu, like "Alt+Space" or "Ctrl+Shift+S"
# (modifiers: Ctrl, Alt, Shift, Win). Empty = the Win key only.
hotkey = ""

# The pinned grid is never wider than this many icons (3 to 8).
max_columns = 6

# Show app names under the pinned icons.
show_names = true

# Start Glint when you sign in to Windows. You can also switch this with
# "Start with Windows" in the tray icon's menu.
autostart = false

[background]
# "solid", "gradient", "glass" (frosted glass) or "glass-gradient".
mode = "glass-gradient"
# Colors: "theme" (the theme's own), "midnight", "aurora", "sunset",
# "ocean", "plain", or "custom" to use `colors` below.
preset = "theme"
# Used when preset = "custom": 1 to 3 colors.
colors = ["#2b1a4a", "#1a2347", "#0f2a3a"]
# Gradient direction in degrees (0 = up, 90 = right).
angle = 135
# How much to darken the background so text stays readable (0.0 to 0.6).
# Glint raises it by itself if the colors are too bright for white text.
overlay = 0.2
# Glass modes: how strongly the colors tint the glass
# (0.0 = clear glass, 1.0 = you can't see through it).
tint = 0.35
# While music plays, tint the background with the album cover's color.
tint_from_album = false

[music]
# false = never show the music card.
enabled = true

[animation]
# How long opening and closing take, in milliseconds (0 to 3000).
open_ms = 340
close_ms = 170
# Delay between one pinned icon popping in and the next (0 to 500 ms).
stagger_ms = 18
# Bounce at the end of the open animation (1.0 = none, up to 2.0).
overshoot = 1.25

# Written by Glint, not by you: how often you launched each app from the
# menu (search ranking uses it). Safe to delete.
[stats.launch_counts]
```

Delete the file to get the defaults back. Errors are also written to the log
at `%LOCALAPPDATA%\Glint\log.txt`.

## How it's built

Glint is a single Rust program that talks to Windows directly through
[windows-rs](https://github.com/microsoft/windows-rs), Microsoft's official
Rust bindings.

- **No web browser inside.** Many good-looking launchers run on WebView2 or
  Electron, which means a whole browser engine in memory. Glint draws with
  the same native building blocks Windows uses for its own interface, which
  is why it needs a few MB instead of a few hundred.
- **Windows.UI.Composition (the "Visual Layer")** holds the menu as a tree
  of visuals inside a plain Win32 window. The open/close curves are
  `KeyFrameAnimation`s with cubic-bezier easing, and the frosted glass is a
  `HostBackdropBrush` (a live blur of what's behind the menu). The compositor
  (DWM) plays the animations itself, at the monitor's refresh rate, so Glint
  doesn't run a frame loop of its own.
- **Direct2D and DirectWrite** draw the text and icons into composition
  surfaces once; they are redrawn only when something changes.
- **A low-level keyboard hook** watches for a lone Win key press. When Win is
  used with another key (Win+E, Win+D...), Glint lets the shortcut through
  to Windows untouched.
- **The media session API** (`GlobalSystemMediaTransportControlsSessionManager`,
  the same one behind the volume flyout) powers the music player. It is
  event-driven: nothing is checked on a timer.
- **The shell's AppsFolder** lists every installed app, classic and Store,
  and Glint is told when apps are installed or removed.
- **The config** is parsed with `serde` and `toml_edit`. `toml_edit` keeps
  your comments when Glint saves pins or launch counts back to the file.

## Build from source

You need:

- [Rust](https://rustup.rs/) (stable, 1.85 or newer, MSVC toolchain)
- Visual Studio Build Tools with the "Desktop development with C++"
  workload (for the MSVC linker and the Windows SDK)

```powershell
git clone https://github.com/ZulFath1/Glint.git
cd Glint
cargo build --release
```

The exe is at `target\release\glint.exe`. To run the checks that CI runs:

```powershell
cargo fmt --check
cargo clippy -- -D warnings
cargo test
```

To run the leak test: `target\release\glint.exe --stress 1000`. It takes about
10 minutes and writes the result to `%LOCALAPPDATA%\Glint\log.txt`.

## Known limitations

- Clicking the real Start button opens the Windows Start menu, not Glint.
- Search finds apps only (no files, settings or web results).
- Settings can't record hotkeys that use the Win key (Windows handles
  those first); write them in `config.toml` instead.
- Pins can't be reordered by dragging; edit their order in `config.toml`.
- Glint has been tested on one PC so far. Multiple monitors, display scales
  other than the author's, a taskbar on the top or side, and real
  Chinese/Japanese/Korean IMEs have had little real-world testing.
- In the 1,000-cycle leak test, memory grows about 1.2 MB, slightly over the
  1 MB goal (details under [Performance](#performance)).
- The exe is not code-signed, so SmartScreen warns on first run.

## Roadmap

Ideas for after 0.1.0, not promises:

- Drag to reorder pins
- Opening Glint from the taskbar's Start button
- Getting the leak test fully under 1 MB
- More testing on multi-monitor and high-DPI setups

## License

[MIT](LICENSE)

## Credits

- Inspired by [Seelen UI](https://github.com/eythaann/Seelen-UI).
- Theme palettes from [Catppuccin](https://github.com/catppuccin/catppuccin)
  (Mocha), [Nord](https://www.nordtheme.com/),
  [Tokyo Night](https://github.com/folke/tokyonight.nvim) and
  [Gruvbox](https://github.com/morhetz/gruvbox), all MIT licensed. Glint
  uses their colors only; the themes are not affiliated with these projects.
- Built on [windows-rs](https://github.com/microsoft/windows-rs),
  [serde](https://serde.rs/) and
  [toml_edit](https://github.com/toml-rs/toml).
- Written with help from Claude Code.

## Feedback

Have a suggestion? Open an [issue](https://github.com/ZulFath1/Glint/issues)
or send me an email: 2410817210011@mhs.ulm.ac.id
