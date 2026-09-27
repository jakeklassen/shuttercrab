# Architecture

The specification is [PRD.md](PRD.md). The color decisions (PRD §37, items 3
and 4) are recorded in [COLOR_PIPELINE.md](COLOR_PIPELINE.md); this file
records the others as they are made.

## Crates

| Crate | Owns | PRD §30 equivalent |
|---|---|---|
| [`framecut`](../crates/framecut) | The GPUI Kit application: the selection overlay and the screenshot flow | `app` |
| [`framecut-capture`](../crates/framecut-capture) | Monitors and their color state, Windows.Graphics.Capture, the HDR/WCG → SDR transform, cropping and PNG encoding, behind the capture service | `capture-core`, `color`, `screenshot` (without clipboard) |
| [`framecut-platform`](../crates/framecut-platform) | Global hotkeys, the clipboard, window behaviour applied to GPUI windows' `HWND`s | `windows-platform` |
| [`capture-spike`](../crates/capture-spike) | Milestone 0's measurement tools and the HDR-off / HDR-on gate | — |

PRD §30 allows simpler boundaries. `capture-core`, `color` and `screenshot`
share one Direct3D device and one thread, so they are one crate; the
clipboard sits with the other Win32 integration in `framecut-platform`.

## ADR 1 — Windows.Graphics.Capture

Accepted (Milestone 0). `CreateForMonitor` with a free-threaded frame pool in
`R16G16B16A16_FLOAT` delivers the compositor's scRGB surface before any 8-bit
conversion, which the color contract needs (PRD §8.5, §9.2). GDI/BitBlt and
DXGI Desktop Duplication give 8-bit or display-signal data and are not used.
One frame is taken per screenshot; no capture runs while idle (§22.1).

## ADR 2 — Direct3D 11

Accepted (Milestone 0). WGC hands out D3D11 textures and GPUI's Windows
backend renders with D3D11 on `windows` 0.62, the version used here, so
textures can be shared with GPUI later without a D3D12 interop layer. The
transform runs as compute shaders compiled at start-up.

## ADR 5 — PNG encoding with the `png` crate

Accepted. The `png` crate writes 8-bit RGBA with an sRGB chunk (and the gAMA
and cHRM fallbacks), needs no COM, and crops and encodes in under a
millisecond for a 640×360 cut (measured, including the crop) at
`Compression::Fast`. WIC stays an option if profiling ever
favours it.

## ADR 7 — The UI/capture boundary

Accepted (Milestone 1). The app talks to two services, each on its own
thread:

- **Capture service** (`framecut_capture::Capture`). One thread owns the
  WinRT apartment, the Direct3D device per adapter and the compiled shaders.
  The app awaits `freeze_monitor` and `screenshot`; no COM or Direct3D object
  crosses to it (PRD §19, §38 rule 12). `warm_up` at launch creates the
  devices and loads WGC, so the first hotkey is as fast as the rest.
- **Platform service** (`framecut_platform::Platform`). One thread with a
  message-only window owns the global hotkeys and writes the clipboard (which
  needs an owner window).

Awaiting their replies never blocks the GPUI main thread (§17, §38 rule 21).
Window behaviour GPUI does not expose is applied to the overlay's `HWND`,
taken through `HasWindowHandle` (§17).

**The overlay shows exactly what is copied.** A `FrozenFrame` holds the
monitor already converted to SDR. The overlay displays those pixels, and the
screenshot is cut from them after selection. PRD §10 sketches "crop, then
convert"; converting the whole monitor once instead costs one full-frame
conversion (≈ 10 ms on the GPU) and guarantees the selection looks the same
before and after release, and that HDR regions are found from whole images,
not from the part a selection happens to cut. The FP16 source is not kept:
nothing uses it yet, and 66 MB per frozen frame is not free (§23).

`FrozenFrame::preview_bgra` hands the UI bytes rather than the `RenderImage`
PRD §19 sketches, so the capture crate does not depend on GPUI; the app wraps
the bytes.

## ADR 8 — Coordinates and DPI

Accepted (Milestone 1).

- **Physical pixels** everywhere in capture and platform code: monitor bounds
  in virtual-desktop coordinates, regions relative to their monitor (PRD
  §20). The capture thread makes itself per-monitor-v2 DPI aware, so this
  holds whatever the host process declares; the app declares it too, through
  GPUI's manifest.
- **Logical pixels** in UI code, as GPUI reports them.
- The conversion happens in one place, `framecut::selection`: each corner of
  a drag snaps to the nearest physical pixel (`round(logical × scale)`) and
  the rectangle is clamped to the monitor. Pointer positions are physical
  pixels over the scale, so they convert back exactly.
- The overlay window opens on the monitor's GPUI display (whose id is the
  `HMONITOR`) with logical bounds, then is pinned to the exact physical
  bounds with `SetWindowPos`, so fractional scale factors leave no gap.

## ADR 9 — Monitors (Milestone 1)

Accepted for now. A screenshot covers the monitor under the pointer when the
hotkey is pressed; a drag stops at that monitor's edges (PRD §21, MVP).
Cross-monitor areas remain post-MVP.

## Defaults awaiting settings (Milestone 2)

- Screenshot: **Ctrl+Alt+S**. Win+Shift+S belongs to Windows (PRD §7.1) and
  PrintScreen to Snipping Tool by default.
- Quit: **Ctrl+Alt+Shift+Q**, until the tray icon exists.
- A second instance finds the screenshot hotkey taken, says so and exits.
- Diagnostics go to stderr; debug builds have a console, release builds do
  not. A log file comes with Milestone 2.
