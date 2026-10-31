# Shuttercrab

A Windows 11 tray app for screenshots, screen recording and live streaming,
written in Rust with [GPUI Kit](https://gpui-kit.com). It works like
Snipping Tool (one hotkey, drag, paste) and produces SDR output that looks
right when Windows HDR is enabled.

The original specification is [docs/PRD.md](docs/PRD.md); the app has
grown past it.

## What it does

- **Screenshots** of an area, a window, a display or a freeform shape. The
  screen freezes first, so what you select is what you get. The screenshot
  goes on the clipboard, is saved as a PNG, and shows a thumbnail to open
  or drag into another app.
- **Markup** in the main window: the pen, the highlighter, the eraser,
  shapes, emoji and crop, with undo and redo.
- **Text actions**: read a screenshot's text with Windows' own text
  recognition, on the device, then copy it, or black out email addresses,
  phone numbers or the words you pick.
- **Recording** of an area, a window or a display to an H.264 MP4 in SDR,
  with the speakers' sound and a microphone if you turn them on. The
  controls and the border round the area are never in the video. The main
  window plays recordings back.
- **Live streaming** of an area, a window or a display to someone
  anywhere, who watches in Shuttercrab from a link. Windows only for now.

**Linux** has no release yet. Under X11, Shuttercrab takes screenshots and
records (the picture only, through GStreamer), with the tray, global
hotkeys, the clipboard and starting at sign-in. Text actions, playing a
recording and streaming are Windows only. Under Wayland it cannot read the
screen yet. CI builds and tests Linux and macOS, and each pull request gets
a Linux build to try. macOS has no backend: Shuttercrab builds there and
says capture is not supported.

Documentation:

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): crates, processes, the UI/capture
  boundary, coordinates and DPI, and other decisions.
- [docs/COLOR_PIPELINE.md](docs/COLOR_PIPELINE.md): the color transform, its
  equations, and the measurements behind it.
- [docs/TEST_MATRIX.md](docs/TEST_MATRIX.md): automated checks, the
  Milestone 0 gate, and the Milestone 1 acceptance test.
- [docs/STREAMING.md](docs/STREAMING.md): live streaming, watching, host
  keys and running the meeting point.

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
in the tray; **Open Shuttercrab** in the tray menu brings it back.

| Keys | Does |
|---|---|
| **Ctrl+Alt+C** | Open the Capture Bar: **S**creenshot, **R**ecord or stream (**L**), then **A**rea, **W**indow, **D**isplay or **F**reeform (click, arrow keys and Enter, or the letter). It starts on your last choice |
| **Ctrl+Alt+S** | Straight to an area: freeze every monitor, drag on any of them, release to copy. Edges snap to nearby windows |
| **Space** while selecting | Switch to Window mode: click a window to capture it, or the desktop for the whole display. Space again returns to Area |
| Escape or right-click | Cancel the selection |
| **Ctrl+Alt+R** | Record an area (drag it); again to stop |
| While recording | **Ctrl+Alt+P** pause and resume, **Ctrl+Alt+N** restart, **Ctrl+Alt+D** discard (each asks first unless turned off in Settings; then **Ctrl+Alt+Z** undoes) |
| Click the tray icon | Open the Shuttercrab window |
| Right-click the tray icon | Open Shuttercrab, capture, record, stream or watch, open the screenshots folder, turn saving on or off, Settings…, quit |
| Ctrl+Alt+Shift+Q | Quit (for development) |

Each way of starting Shuttercrab does one thing. While a copy is running,
a new start passes the request to it and exits.

| Start | Does |
|---|---|
| From the Start menu, or `shuttercrab.exe` | Shows the Shuttercrab window |
| `shuttercrab.exe --capture-bar`, `--screenshot` or `--record` | Acts as that hotkey would: opens the Capture Bar, takes an area screenshot, or starts an area recording (or stops the one running). For desktop shortcuts |
| At sign-in, if turned on in Settings | The tray only, with no window |
| A `shuttercrab://` link, in an installed copy | Opens the viewer for that stream, in a process of its own (see [docs/STREAMING.md](docs/STREAMING.md)) |

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

**L** streams live and **J** watches a stream: see
[docs/STREAMING.md](docs/STREAMING.md).

Setting `SHUTTERCRAB_DATA_DIR` keeps settings and logs in that folder instead,
for testing.

## Setup

Rust, and CMake for the Opus library a stream's sound needs on Windows,
are pinned with [mise](https://mise.jdx.dev) in `mise.toml`.

```powershell
mise install
mise run check
```

`mise run check` runs formatting, Clippy and the tests as CI does. It
skips one test, which would signal the Shuttercrab you have running, and
first empties `target/` if it has grown past 30 GiB. To run one crate's
tests, or one test, use Cargo through mise:

```powershell
mise exec -- cargo test -p shuttercrab-capture
mise exec -- cargo test -p shuttercrab --test ui overlay::
```

The measurement and diagnostic tools remain: `mise exec -- cargo run
--release -p capture-spike -- help`.

## Releases

Commits on `main` follow [Conventional Commits](https://www.conventionalcommits.org).
On every push, [release-plz](https://release-plz.dev) keeps a Release PR up
to date with the next version and its [CHANGELOG.md](CHANGELOG.md) entry;
only `feat`, `fix`, `perf`, `refactor` and `revert` commits call for one.
Merging that PR runs CI, tags `vX.Y.Z`, builds the installer and update
packages with [Velopack](https://velopack.io), and publishes them to GitHub
Releases. The app and its five library crates share one version, one tag
and one changelog, since a change in a library is a change in the app;
`capture-spike` is never released. The settings are in `release-plz.toml`
and the workflows in `.github/workflows`.

To try an update before publishing it, point an installed copy at a folder
of packages built with `vpk pack`: set `SHUTTERCRAB_UPDATE_SOURCE` to that
folder before starting it.

## Licence

Shuttercrab is licensed under either of [MIT](LICENSE-MIT) or
[Apache-2.0](LICENSE-APACHE), at your option. The patched copy of GPUI's
Windows layer in `vendor/gpui-pre-windows` keeps its own Apache-2.0 licence.
