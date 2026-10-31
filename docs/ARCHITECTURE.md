# Architecture

The original specification is [PRD.md](PRD.md). The color decisions (PRD
§37, items 3 and 4) are recorded in [COLOR_PIPELINE.md](COLOR_PIPELINE.md);
this file records the others as they are made. Live streaming, its meeting
point and its keys are described in [STREAMING.md](STREAMING.md).

## Crates

| Crate | Owns | PRD §30 equivalent |
|---|---|---|
| [`shuttercrab`](../crates/shuttercrab) | The GPUI Kit application: the tray app and its main window, the selection overlay, the Capture Bar, the thumbnail, markup and text actions on a screenshot, recording and its controls, playing a recording, settings and updates. The same program runs the recording, streaming and viewer helpers (see [Processes](#processes)) | `app` |
| [`shuttercrab-capture`](../crates/shuttercrab-capture) | Monitors and their color state, capture, the HDR/WCG → SDR transform, cropping and PNG encoding behind the capture service; recording and playback; a live recording's frames and playing them as they arrive | `capture-core`, `color`, `screenshot` (without clipboard) |
| [`shuttercrab-platform`](../crates/shuttercrab-platform) | OS integration GPUI does not provide: global hotkeys, the tray, notifications, the clipboard, window behaviour applied to GPUI windows, text recognition, single instance, starting at sign-in, links that open Shuttercrab, kept secrets, helper processes, and what the OS is called | `windows-platform` |
| [`shuttercrab-types`](../crates/shuttercrab-types) | The window and monitor identities the other crates pass between them | — |
| [`shuttercrab-connect`](../crates/shuttercrab-connect) | Live streaming's connection: a streaming app and a viewer meet in a room on the meeting point (`meet/`), then connect with QUIC, end to end encrypted, through Cloudflare's TURN relay by default | — |
| [`shuttercrab-stream`](../crates/shuttercrab-stream) | Live streaming: a recording sent to a viewer as it is made, the window that plays it, and the stream's quality and codec choice | — |
| [`capture-spike`](../crates/capture-spike) | Milestone 0's measurement tools and the HDR-off / HDR-on gate, and command-line tools for recording, playback, text recognition and streaming | — |

PRD §30 allows simpler boundaries. `capture-core`, `color` and `screenshot`
share one Direct3D device and one thread, so they are one crate; the
clipboard sits with the other OS integration in `shuttercrab-platform`.

## Processes

Shuttercrab is one program. Started with one of these flags first on its
command line, it runs as a helper instead of the app: no tray and no
single-instance check. Velopack's install and update hooks never run in a
helper, so an update cannot end a recording or a stream.

| Flag | Module | What it does | Why it is a process of its own |
|---|---|---|---|
| `--recorder` | [`recorder_process.rs`](../crates/shuttercrab/src/recorder_process.rs) | Makes one recording, then exits | The hardware encoder's driver keeps hundreds of megabytes of system memory for as long as the process that used it lives (about 265 MB after a 4K recording). The helper exits when the recording ends and takes that memory with it |
| `--streamer` | [`stream_process.rs`](../crates/shuttercrab/src/stream_process.rs) | Opens a room on the meeting point, says the link to share, waits for a viewer and streams to it until asked to stop or the viewer leaves. Windows only | As for recording, the encoder's memory goes with the helper; and the app never waits on the network |
| `--watch LINK` | [`watch.rs`](../crates/shuttercrab/src/watch.rs) | The viewer: a window that plays the stream the link is for. Windows only | It runs beside the app or without it. The app's Watch box starts one, and so does Windows for a `shuttercrab://` link clicked anywhere, once the installer has registered the scheme |

The app starts the recorder and the streamer with their standard input,
output and error as pipes (`shuttercrab_platform::process`; on Windows it
also keeps the busy pointer off, which was otherwise recorded). They talk in
JSON lines: requests go in on standard input, replies come out on standard
output. The helper's log lines come over standard error and go into the
app's log. If the app goes away, the helper's input ends and it stops; a
recording is finished as if asked.

The viewer does not talk to the app. It writes its own log, `viewer.log`,
in the app's log folder.

Starting the app itself while it already runs does not start a second one:
the new start passes its request (show the window, or act as a hotkey) to
the running one and exits.

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

- **Capture service** (`shuttercrab_capture::Capture`). One thread owns the
  WinRT apartment, the Direct3D device per adapter and the compiled shaders.
  The app awaits `list_monitors`, `freeze_monitor` and `capture_window`; no
  COM or Direct3D object crosses to it (PRD §19, §38 rule 12). A screenshot
  of an area or a shape is cut from a frozen monitor with `cut` or
  `cut_shape`, off the UI thread. `warm_up` at launch creates the devices
  and loads WGC, so the first hotkey is as fast as the rest.
- **Platform service** (`shuttercrab_platform::Platform`). On Windows, one
  thread with a hidden window owns the tray icon, the global hotkeys, the
  clipboard (which needs an owner window) and the watching of a recorded
  window. It is a hidden top-level window rather than a message-only one,
  because a message-only window misses the `TaskbarCreated` broadcast and
  loses its tray icon when Explorer restarts.

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

`FrozenFrame::bgra` (or `into_bgra`, without a copy) hands the UI bytes
rather than the `RenderImage` PRD §19 sketches, so the capture crate does
not depend on GPUI; the app wraps the bytes.

## ADR 8 — Coordinates and DPI

Accepted (Milestone 1).

- **Physical pixels** everywhere in capture and platform code: monitor bounds
  in virtual-desktop coordinates, regions relative to their monitor (PRD
  §20). The capture thread makes itself per-monitor-v2 DPI aware, so this
  holds whatever the host process declares; the app declares it too, through
  GPUI's manifest.
- **Logical pixels** in UI code, as GPUI reports them.
- The conversion happens in one place, `shuttercrab::selection`: each corner of
  a drag snaps to the nearest physical pixel (`round(logical × scale)`) and
  the rectangle is clamped to the monitor. Pointer positions are physical
  pixels over the scale, so they convert back exactly.
- The overlay window opens on the monitor's GPUI display (whose id is the
  `HMONITOR`) with logical bounds, then is pinned to the exact physical
  bounds with `SetWindowPos`, so fractional scale factors leave no gap.

## ADR 9 — Monitors (Milestone 1)

Accepted for now. A selection stays on one monitor: a drag stops at that
monitor's edges (PRD §21, MVP). Milestone 1 froze only the monitor under the
pointer. Now every monitor is frozen, each in its own overlay: the one under
the pointer first, and it takes the keyboard. Cross-monitor areas remain
post-MVP.

## ADR 10 — One backend per OS (issue #49)

Accepted. Shuttercrab ships for Windows, and macOS and Linux are to
follow. The capture and platform crates each have one public module per
area that is the same on every OS, and an OS backend under `src/sys`:

| Crate | Public, the same on every OS | `sys/windows` | `sys/linux` |
|---|---|---|---|
| `shuttercrab-capture` | `screen` (rectangles, monitors, frozen frames, screenshots, errors, cutting, PNG), `service` (the capture handle), `color`, `shape`, `raw`, `png_io`; `record` (options, summary, interruptions, sources, microphones, exposure smoothing, sizes); `play` (the player's events); `live` (a live recording's frames and their wire format) | the capture service, Windows.Graphics.Capture, Direct3D 11 and the shaders, Media Foundation recording and playback, WASAPI sound, Opus, playing a live recording as it arrives | under X11: monitors from RandR and screenshots read from the root window; recording, picture only, encoded to H.264 by GStreamer. Playback is the unsupported backend's, and window capture fails, so the app cuts the window from the frozen screen instead |
| `shuttercrab-platform` | the platform service's events, tray and hotkey types, and requests from a second start; `Hotkey` and `Key` (parsing); `frame` (the border's strips and dashes); `targets`, `window`, `cursor`, `drag`, `icon`, `memory`, `ocr` (text types), `os`, `open`, `process`, `startup`, `launcher`, `links`, `secret` | the hidden window: tray, hotkeys, clipboard, notifications; single instance, WinEvent watching, layered windows, OLE drag, cursors, Windows.Media.Ocr, the registry (starting at sign-in, the `shuttercrab://` scheme), DPAPI, helper processes without the busy pointer, attaching to the terminal | single instance over a Unix socket, the tray (StatusNotifierItem), notifications, the clipboard, helper processes, memory figures, the launcher entry, starting at sign-in (XDG autostart); under X11, global hotkeys, the recording border, window placement and the windows on screen. Cursors, drag, OCR, opening in another program, links, secrets, window watching and the terminal are the unsupported backend's |

`src/sys/mod.rs` picks the backend as `sys::imp`, and the public modules
re-export the rest from it, so the app's paths are the same on every OS.
An OS with no backend yet gets `sys/unsupported`: the app builds and
starts, and every capture, recording, playback and system call fails or
does nothing, saying it is not supported there
(`CaptureErrorCode::Unsupported`). macOS has only that backend. Linux has
its own, `sys/linux`, and takes from `sys/unsupported` what it does not
have yet. Under Wayland, Linux cannot read the screen, grab global hotkeys
or place windows; there, desktop shortcuts run Shuttercrab's requests in
place of hotkeys. CI builds and tests on Linux and macOS to keep it that
way.

Windows and monitors cross the crates as `shuttercrab_types::WindowId` and
`MonitorId`: opaque, made and read only by a backend, and never saved. On
Windows they are the `HWND` and the `HMONITOR`, which GPUI also uses as its
display id. Under X11 a window is its X11 window id and a monitor is its
RandR name; GPUI there has one display, the whole screen, so `popup.rs`
gives it no display id. The recorder process receives them as numbers. A
hotkey's key is a `Key`, which the Windows backend turns into a virtual-key
code and the Linux one into an X11 keycode. What the user is told about the
OS (its name and version, its image editor, its default microphone, its
Print Screen setting, whether it has global hotkeys and a tray) comes from
`os()`.

Traits were considered and not used: the services already sit behind
handles the app holds, and when this was decided only one backend was real.
Separate per-OS crates were too: they would add crates to release before
there is a second backend to put in them.
