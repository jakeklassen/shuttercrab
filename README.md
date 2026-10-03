# Shuttercrab

A Windows 11 screenshot and screen-recording utility, written in Rust with
[GPUI Kit](https://gpui-kit.com). It works like Snipping Tool (one hotkey, drag,
paste) and produces SDR output that looks right when Windows HDR is enabled.

The specification is [docs/PRD.md](docs/PRD.md).

## Status

- **Milestone 0** (prove HDR-to-SDR color correctness) passed on 2026-09-27.
- **Milestone 1** (area screenshot MVP) passed its acceptance test on 2026-09-27:
  hotkey → drag → release → paste.
- **Milestone 2** (screenshot workflow) is complete (2026-09-28). Step 1 of 6
  (tray icon, settings, log file, auto-save, single instance) passed its
  acceptance test on 2026-09-27. Step 2 (window and display capture, Space
  toggle, snapping) passed on 2026-09-28. Step 3 (the Capture Bar) passed on
  2026-09-28. Step 4 (thumbnail, notifications, capture exclusion)
  passed on 2026-09-28. Step 5 (the settings window) passed on
  2026-09-28. Step 6 (hardening, HDR video) passed on 2026-09-28; sleep and
  wake is left for the owner to try.
- **Milestone 3** (video technical spike) passed on 2026-09-29:
  `capture-spike record` writes H.264 MP4 through the GPU pipeline.
- **Milestone 4** (area recording) is complete (2026-10-01): record an area
  or a display from the Capture Bar or Ctrl+Alt+R, with controls that are
  never in the video (pause, stop, restart, discard, each with a hotkey),
  a dashed border round the area, an optional countdown, and recording
  settings. Steps 1–4 passed on 2026-09-29, 2026-09-30 and 2026-10-01.

Documentation:

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): crates, the UI/capture
  boundary, coordinates and DPI, and other decisions.
- [docs/COLOR_PIPELINE.md](docs/COLOR_PIPELINE.md): the color transform, its
  equations, and the measurements behind it.
- [docs/TEST_MATRIX.md](docs/TEST_MATRIX.md): automated checks, the
  Milestone 0 gate, and the Milestone 1 acceptance test.

## Install

Download `Shuttercrab-win-Setup.exe` from the
[latest release](https://github.com/jakeklassen/shuttercrab/releases/latest)
and run it. It installs for your user only (no administrator rights), adds
Shuttercrab to the Start menu and starts it. Installed copies update
themselves: when a new release is downloaded, the tray menu offers
"Restart to update". Uninstall from Windows Settings, Apps.

## Use it

To run it from the source instead:

```powershell
mise exec -- cargo run --release -p shuttercrab
```

Starting Shuttercrab opens its window: **N** (or New) starts a capture,
**S** or **R** picks screenshot or recording, **A**, **W**, **D** or **F**
the target, **T** the delay, and **,** opens Settings in the same window.
The window hides while you capture. Closing it leaves Shuttercrab running
in the tray; **Open Shuttercrab** in the tray menu brings it back. Started at sign-in,
it starts in the tray only. Only one copy runs at a time.

| Keys | Does |
|---|---|
| **Ctrl+Alt+C** | Open the Capture Bar: **S**creenshot or **R**ecord, then **A**rea, **W**indow, **D**isplay or **F**reeform (click, arrow keys and Enter, or the letter). It starts on your last choice |
| **Ctrl+Alt+S** | Straight to an area: freeze every monitor, drag on any of them, release to copy. Edges snap to nearby windows |
| **Space** while selecting | Switch to Window mode: click a window to capture it, or the desktop for the whole display. Space again returns to Area |
| Escape or right-click | Cancel the selection |
| **Ctrl+Alt+R** | Record an area (drag it); again to stop |
| While recording | **Ctrl+Alt+P** pause and resume, **Ctrl+Alt+N** restart, **Ctrl+Alt+D** discard (each asks first unless turned off in Settings; then **Ctrl+Alt+Z** undoes) |
| Click the tray icon | Open the Shuttercrab window |
| Right-click the tray icon | Open Shuttercrab, capture, Settings…, open the screenshots folder, turn saving on or off, quit |
| Start Shuttercrab again | Opens its settings window |
| Ctrl+Alt+Shift+Q | Quit (for development) |

The screenshot is on the clipboard as PNG and as a bitmap; paste it anywhere.
Window captures keep the window's rounded corners transparent in the PNG
(the bitmap has them on white).
It is also saved to `Pictures\Shuttercrab` as `Capture YYYY-MM-DD HH-MM-SS.png`.
A thumbnail appears in the corner for a few seconds: click it to open the
image, or drag it into a chat, browser or folder. It never takes the
keyboard, so Ctrl+V still pastes where you were.

Recordings are saved to `Videos\Shuttercrab` as `Recording YYYY-MM-DD
HH-MM-SS.mp4` (H.264, SDR). While recording, a small bar beside the area
shows the time and the actions, and a dashed border marks the area;
neither is ever in the video. All the keys can be changed in Settings.

| File | Where |
|---|---|
| Settings | `%APPDATA%\Shuttercrab\settings.json` |
| Log (this run, and the previous one) | `%LOCALAPPDATA%\Shuttercrab\logs` |

Setting `SHUTTERCRAB_DATA_DIR` keeps settings and logs in that folder instead,
for testing.

## Setup

Rust is pinned with [mise](https://mise.jdx.dev) in `mise.toml`.

```powershell
mise install
mise exec -- cargo clippy --all-targets -- -D warnings
mise exec -- cargo test
```

The Milestone 0 tools remain: `mise exec -- cargo run --release -p
capture-spike -- help`.

## Releases

Commits on `main` follow [Conventional Commits](https://www.conventionalcommits.org).
On every push, [release-plz](https://release-plz.dev) keeps a Release PR up
to date with the next version and its [CHANGELOG.md](CHANGELOG.md) entry;
only `feat`, `fix`, `perf`, `refactor` and `revert` commits call for one.
Merging that PR runs CI, tags `vX.Y.Z`, builds the installer and update
packages with [Velopack](https://velopack.io), and publishes them to GitHub
Releases. The app and its two library crates share one version; the
workflows are in `.github/workflows`.

To try an update before publishing it, point an installed copy at a folder
of packages built with `vpk pack`: set `SHUTTERCRAB_UPDATE_SOURCE` to that
folder before starting it.

## Licence

Shuttercrab is licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. The patched copy of GPUI's
Windows layer in `vendor/gpui-pre-windows` keeps its own Apache-2.0 licence.
