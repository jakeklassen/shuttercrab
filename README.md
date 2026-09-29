# Framecut

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

Documentation:

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md): crates, the UI/capture
  boundary, coordinates and DPI, and other decisions.
- [docs/COLOR_PIPELINE.md](docs/COLOR_PIPELINE.md): the color transform, its
  equations, and the measurements behind it.
- [docs/TEST_MATRIX.md](docs/TEST_MATRIX.md): automated checks, the
  Milestone 0 gate, and the Milestone 1 acceptance test.

## Use it

```powershell
mise exec -- cargo run --release -p framecut
```

Framecut runs in the tray with no window. Only one copy runs at a time.

| Keys | Does |
|---|---|
| **Ctrl+Alt+C** or click the tray icon | Open the Capture Bar: **A**rea, **W**indow or **D**isplay (click, arrow keys and Enter, or the letter). It starts on your last choice |
| **Ctrl+Alt+S** | Straight to an area: freeze the monitor under the pointer, drag, release to copy. Edges snap to nearby windows |
| **Space** while selecting | Switch to Window mode: click a window to capture it, or the desktop for the whole display. Space again returns to Area |
| Escape or right-click | Cancel the selection |
| Right-click the tray icon | Settings…, open the screenshots folder, turn saving on or off, quit |
| Start Framecut again | Opens its settings window |
| Ctrl+Alt+Shift+Q | Quit (for development) |

The screenshot is on the clipboard as PNG and as a bitmap; paste it anywhere.
Window captures keep the window's rounded corners transparent in the PNG
(the bitmap has them on white).
It is also saved to `Pictures\Framecut` as `Capture YYYY-MM-DD HH-MM-SS.png`.
A thumbnail appears in the corner for a few seconds: click it to open the
image, or drag it into a chat, browser or folder. It never takes the
keyboard, so Ctrl+V still pastes where you were.

| File | Where |
|---|---|
| Settings | `%APPDATA%\Framecut\settings.json` |
| Log (this run, and the previous one) | `%LOCALAPPDATA%\Framecut\logs` |

Setting `FRAMECUT_DATA_DIR` keeps settings and logs in that folder instead,
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
