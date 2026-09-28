# Framecut

A Windows 11 screenshot and screen-recording utility, written in Rust with
[GPUI Kit](https://gpui-kit.com). It works like Snipping Tool (one hotkey, drag,
paste) and produces SDR output that looks right when Windows HDR is enabled.

The specification is [docs/PRD.md](docs/PRD.md).

## Status

- **Milestone 0** (prove HDR-to-SDR color correctness) passed on 2026-09-27.
- **Milestone 1** (area screenshot MVP) passed its acceptance test on 2026-09-27:
  hotkey → drag → release → paste.
- **Milestone 2** (screenshot workflow) is in progress. Step 1 of 6
  (tray icon, settings, log file, auto-save, single instance) passed its
  acceptance test on 2026-09-27.

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
| **Ctrl+Alt+S** or click the tray icon | Freeze the monitor under the pointer; drag an area; release to copy it |
| Escape or right-click | Cancel the selection |
| Right-click the tray icon | Open the screenshots folder, turn saving on or off, quit |
| Ctrl+Alt+Shift+Q | Quit (for development) |

The screenshot is on the clipboard as PNG and as a bitmap; paste it anywhere.
It is also saved to `Pictures\Framecut` as `Capture YYYY-MM-DD HH-MM-SS.png`.

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
