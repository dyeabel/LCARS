# LCARS Wallpaper

An LCARS interface running as an animated Windows desktop wallpaper: behind the
desktop icons, out of Alt+Tab and the taskbar, controlled entirely from a tray
icon.

Built with Tauri 2 (Rust + WebView2). The interface itself is the unmodified
renderer bundle from meWho's [Titan.DS](https://www.mewho.com/titan) desktop
app; this project replaces the Electron shell around it with a native wallpaper
host. Credit and licence terms are at the bottom.

## What it does

- **Runs as the wallpaper.** The windows are re-parented into Explorer's WorkerW,
  so desktop icons stay on top and nothing appears in Alt+Tab or the taskbar.
- **Multi-monitor, three ways.** Switch in the tray:
  - *A separate scene on each screen* - every monitor runs its own instance and
    can show a different module.
  - *One scene split across screens* - a single interface laid out over the
    whole desktop, with each monitor showing its own part of it. Every screen keeps its own window, so they pause independently.
  - *One window stretched over all screens* - the same continuous scene in a
    single window spanning the desktop.
- **Not interactive.** The windows ignore the mouse entirely; everything is set
  from the tray menu, runs on a timer, or pauses itself.
- **Fits around the taskbar.** *Screen fit* can hold the wallpaper inside the
  shell's work area so the taskbar covers none of the interface, and extra edge
  insets handle bars the shell does not report.
- **Pauses for games.** When an application covers a monitor edge to edge, the
  wallpaper on that monitor is hidden until the application goes away. Each
  monitor is handled separately.
- **Survives Explorer restarts.** If the desktop host disappears, the windows are
  rebuilt and re-attached automatically.
- **Follows the screens.** A resolution change, a scaling change or a monitor
  coming and going is noticed within seconds and the windows are laid out again;
  no reload needed. Sizes are handled in physical pixels and converted for the
  page, so display scaling other than 100% lands in the right place.
- **Live readouts.** CPU, memory and network figures in the interface are real.
- **Plays along with Fences.** Anything that tints itself from the wallpaper
  bitmap - Stardock Fences panels above all - cannot see the animation and ends
  up sampling whatever photo is set as the wallpaper. Painting the static
  wallpaper black from the tray puts those panels back in step; per-fence
  opacity stays a Fences setting.

### Screen shapes

The interface is responsive rather than fixed: at 4:3 it keeps its 16:9 layout
and letterboxes, and at ultrawide ratios it reflows and fills the width with
extra panels. Verified from 1024x768 up to 3:1.

## Requirements

- Windows 10 or 11, x64
- WebView2 runtime (preinstalled on Windows 11 and up-to-date Windows 10)

For building: Rust (stable) and the MSVC C++ build tools.

## Running it

```sh
npm install
npm run dev      # debug build with a console for the startup log
npm run build    # release build plus an NSIS installer
```

Artifacts land in `src-tauri/target/release/`; the installer in
`src-tauri/target/release/bundle/nsis/`.

## The tray menu

| Item | What it does |
| --- | --- |
| Pause / Resume | Hides or restores every wallpaper window |
| Shuffle modules now | Rolls a new module on each screen |
| Module | The module every screen uses, or *Random* |
| Module per screen | Per-monitor override, shown when more than one window exists |
| Layout | Separate scenes, one split scene, or one stretched window |
| Screens | All monitors, the primary one, or a hand-picked set |
| Appearance | Margin, filter effect, red alert, boot animation |
| Screen fit | Whole screen, or keep clear of the taskbar |
| Static wallpaper | Leaves the Windows wallpaper alone, or paints it black |
| Audio | Off by default; sound effect and engine volumes |
| Behaviour | Fullscreen pause, live readouts, shuffle interval, start with Windows |
| Reload wallpaper | Rebuilds the windows, e.g. after a display change |
| Settings file | Opens `%APPDATA%\com.lcars.wallpaper\settings.json` |

Modules: Titan (default), Enterprise, Enterprise D, Enterprise F, Enterprise G,
NCC-1031, Warp drive, Starbase, DNA.

### Edge insets

Beyond the taskbar, anything docked to a screen edge can be given room by hand.
`insets` in the settings file trims that many physical pixels off each edge,
after the screen fit has been applied:

```json
"fit": "workarea",
"insets": { "top": 0, "right": 0, "bottom": 40, "left": 0 }
```

Reload the wallpaper from the tray after editing the file.

## How it works

Explorer paints the wallpaper into a `WorkerW` window sitting directly behind the
desktop icon host (`SHELLDLL_DefView`). Sending Progman the undocumented message
`0x052C` makes Explorer create it; the app then re-parents its own windows into
it, which puts them exactly where the wallpaper bitmap would be.

Where that `WorkerW` ends up depends on the Windows build - a top-level sibling
of Progman on Windows 10, a child of Progman on recent Windows 11 - and both
layouts are handled, with Progman itself as a fallback. See
[`src-tauri/src/desktop.rs`](src-tauri/src/desktop.rs).

The web bundle expects Electron's `electronAPI` preload object. The equivalent
bridge is generated per window in
[`src-tauri/src/bridge.rs`](src-tauri/src/bridge.rs) and injected as a WebView
init script: the renderer calls Rust through Tauri's invoke, and Rust pushes
actions back with `eval`. The split layout is done in the same script, by
reporting the whole desktop as the viewport and shifting the document so each
window lands on its own slice.

## Layout

```
app/renderer/        meWho's Titan.DS web bundle (unmodified apart from a stale CSP meta tag)
src-tauri/src/
  main.rs            app setup, commands, the renderer action handler
  wallpaper.rs       window per monitor, layouts, pausing, the supervisor loop
  desktop.rs         Win32: WorkerW discovery, re-parenting, fullscreen detection
  bridge.rs          the electronAPI bridge and split viewport script
  tray.rs            tray icon and menu
  settings.rs        settings file
  stats.rs           CPU / memory / network readouts
```

## Credits and licence

Titan.DS is by **meWho (Rob)** - <https://www.mewho.com/titan>. The interface,
artwork and sounds are his work, licensed
[CC BY-NC 4.0](https://creativecommons.org/licenses/by-nc/4.0/), and this
wallpaper port inherits that licence: attribution required, non-commercial use
only.

If you enjoy it, support the original author on
[Patreon](https://www.patreon.com/mewho) or [Ko-Fi](https://ko-fi.com/system47).
