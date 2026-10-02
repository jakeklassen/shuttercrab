# Windows 11 HDR-Aware Snipping Utility
## Product Requirements Document

**Status:** Implementation-ready  
**Platform:** Windows 11 only  
**Language:** Rust  
**UI framework:** GPUI Kit (`gpui-kit` crate)  
**Platform integration:** Windows APIs through the `windows` crate  
**Primary goal:** A Snipping Tool-like screenshot and screen-recording utility that produces visually correct SDR output when Windows HDR is enabled.

**Product name:** Shuttercrab.

---

# 1. Executive Summary

Build a lightweight Windows 11 screen-capture utility that matches the speed and simplicity of the built-in Windows Snipping Tool while fixing a specific color-management failure mode: screenshots and recordings captured from HDR-enabled displays often look washed out, over-bright, grey, or otherwise incorrect when viewed as ordinary SDR content.

The application must be optimized for day-to-day development workflows:

- invoke instantly from a global hotkey;
- select a region quickly;
- capture either a screenshot or a video;
- preserve visually correct whites, greys, text, UI chrome, and saturated colors;
- copy screenshots directly to the clipboard;
- save screenshots and recordings locally;
- produce output that looks correct in SDR destinations such as GitHub, Slack, Discord, browsers, issue trackers, documentation, and standard image/video viewers.

The application is **HDR-aware**, but the MVP does **not** prioritize HDR export. The initial product contract is:

> Capture whatever Windows is displaying, including HDR/WCG desktop content, and convert it into a visually faithful SDR image or SDR video.

The application should feel closer to Windows Snipping Tool than to ShareX or OBS. It should be fast, transient, minimal, and opinionated.

---

# 2. Problem Statement

Windows 11 can render the desktop through an HDR / Advanced Color composition pipeline. On HDR displays, ordinary SDR application content such as IDEs, terminals, browsers, and white-background web applications is rendered within a wider/high-dynamic-range desktop representation.

A naive screen-capture pipeline can mishandle this by:

- capturing only an 8-bit representation;
- clipping or incorrectly transforming the HDR/WCG framebuffer;
- ignoring the display's SDR reference-white setting;
- converting FP16/scRGB data to 8-bit RGB without proper tone mapping;
- encoding video as ordinary SDR without first transforming the source correctly.

The practical result is especially visible in light themes:

- white backgrounds appear grey, blown out, or over-bright;
- light greys lose contrast;
- text contrast changes;
- UI colors appear desaturated;
- screenshots do not visually match what the user saw;
- recordings can look "white washed."

This makes normal development screenshots and short bug/demo recordings unreliable.

The built-in Windows Snipping Tool is otherwise close to the desired UX, so the goal is not to build a large capture suite. The goal is to reproduce the small, fast interaction model while owning the capture/color pipeline.

---

# 3. Goals

## 3.1 Primary Goals

1. **Correct SDR screenshots from HDR desktops**
   - Light-theme IDEs and browser content must retain visually correct whites, greys, text contrast, and colors.
   - Output should be suitable for ordinary SDR applications and web services.

2. **Correct SDR screen recordings from HDR desktops**
   - The same color correctness requirements apply to video.
   - Recorded output must not appear washed out in standard SDR playback.

3. **Snipping Tool-like speed**
   - Global hotkey.
   - Immediate selection overlay.
   - Minimal clicks.
   - Screenshot copied to clipboard automatically.
   - Recording start/stop should be simple and obvious.

4. **Modern Windows 11-only implementation**
   - No Windows 10 compatibility work.
   - No macOS/Linux abstractions.
   - Prefer current Windows APIs.

5. **Small and maintainable architecture**
   - GPUI Kit owns application UI.
   - Dedicated crates own Windows integration, capture, GPU processing, clipboard, and encoding.
   - Platform implementation details are hidden behind a narrow Rust API.

## 3.2 Secondary Goals

- Window capture in the first complete screenshot release.
- Full-monitor screenshot capture.
- A unified transient Capture Bar for Screenshot / Record and Area / Window / Display selection.
- Space-to-toggle between area and window selection during screenshot targeting.
- Live region dimensions and optional window-boundary snapping.
- Instant clipboard availability plus a transient post-capture thumbnail.
- Drag-and-drop of a completed screenshot directly into other applications.
- Pause/resume/restart/discard controls for recording.
- Exclusion of application-owned capture UI from screenshots and recordings.
- Configurable cursor inclusion.
- Configurable output directory.
- Configurable hotkeys.
- Lightweight tray presence.
- Fast startup or persistent low-overhead background process.

---

# 4. Non-Goals

The following are explicitly out of scope for the first implementation unless required as enabling work:

- Cross-platform support.
- Windows 10 support.
- Cloud uploads.
- Account system.
- Image hosting.
- Full annotation/image editor in MVP. A lightweight capture preview is allowed.
- OCR.
- Scrolling screenshots.
- GIF recording.
- Webcam capture.
- Streaming.
- Scene composition.
- Multiple sources / OBS-style scenes.
- Multi-track video editing.
- System audio capture in MVP.
- Microphone capture in MVP.
- True HDR image export in MVP.
- HDR10 / HLG video export in MVP.
- Cross-monitor selection spanning displays in MVP.
- Capture-history database in MVP.
- Automatic telemetry.

---

# 5. Target User

The primary user is a technical Windows user who:

- uses Windows 11 with one or more HDR-capable monitors;
- frequently captures IDEs, terminals, browsers, design tools, issue trackers, and desktop applications;
- needs screenshots to paste into chat, pull requests, tickets, or documentation;
- records short screen clips for bug reports, demos, and technical communication;
- values speed and low friction over advanced editing features.

The tool should also work normally on SDR monitors, but HDR correctness is the motivating use case.

---

# 6. Product Principles

## 6.1 Correctness Before Features

A capture tool that produces the wrong colors has failed its core job.

Do not add secondary features until the screenshot SDR conversion path is demonstrably correct.

## 6.2 Fast by Default

The user should not need to open a conventional application window before capturing.

Normal screenshot flow:

1. press hotkey;
2. drag region;
3. release;
4. screenshot is on clipboard.

## 6.3 SDR as the Interoperability Format

The MVP should optimize for screenshots and recordings that look correct nearly everywhere.

Default image output:

- PNG
- 8-bit sRGB

Default video output:

- MP4
- H.264
- Rec.709 SDR

## 6.4 Platform Work Stays Out of UI Code

Use GPUI Kit for product UI, but keep GPU capture, Direct3D textures, Windows capture APIs, tone mapping, video encoding, and clipboard internals out of UI code.

Those responsibilities belong in the platform and capture crates.

## 6.5 Capture Utility, Not Presentation Suite

The application may borrow interaction ideas from polished capture tools such as ScreenFlare, but the MVP must remain a fast Windows capture utility.

The core product is:

> invoke → select → capture → share

Do not expand MVP into a timeline editor, Loom replacement, or OBS competitor.

Presentation-oriented features such as automatic zooms, cursor smoothing, keystroke overlays, backgrounds, captions, trimming, and privacy masks belong in a clearly separated future phase.

---

# 7. UX Requirements

# 7.1 Background Behavior

The application should normally run in the background with minimal resource usage.

It must support:

- one configurable global **Capture Bar** hotkey;
- optional direct screenshot hotkey;
- optional direct recording hotkey;
- system tray icon;
- settings access from tray;
- clean quit action.

The Capture Bar is the primary discoverable entry point.

Recommended behavior:

```text
Capture Bar hotkey
    ↓
[ Screenshot ] [ Record ]
[ Area ] [ Window ] [ Display ]
```

Power users should be able to bypass the Capture Bar with direct actions.

Recommended defaults should avoid conflicts with Windows Snipping Tool.

Do not attempt to replace or intercept Windows' reserved `Win+Shift+S` unless Windows provides a reliable supported mechanism and the user explicitly opts in.

---

# 7.2 Screenshot Interaction

Expected area-capture flow:

1. User presses the screenshot hotkey or opens Capture Bar → Screenshot.
2. App identifies the monitor beneath the pointer.
3. App captures/finalizes a frame for that monitor.
4. A full-screen borderless selection overlay appears.
5. Cursor changes to crosshair.
6. User drags a rectangle.
7. Live physical-pixel dimensions are shown near the selection.
8. Window boundaries may subtly snap when the selection is near an application edge.
9. On mouse release:
   - selection overlay closes;
   - capture is cropped;
   - HDR/WCG source is converted to SDR;
   - PNG is encoded;
   - image is copied to clipboard immediately;
   - image is optionally saved to disk;
   - a transient post-capture thumbnail appears;
   - optional lightweight notification is shown.

Target interaction:

> hotkey → drag → release → paste

Clipboard availability must not wait for the thumbnail, preview, notification, or disk-save UI.

No modal confirmation is required.

Escape cancels the selection.

Right-click may cancel if convenient.

---

# 7.3 Area ↔ Window Selection

During screenshot targeting, pressing **Space** should toggle between:

- Area selection
- Window selection

Expected behavior:

### Area mode

- drag arbitrary rectangle;
- show live physical-pixel dimensions;
- optional boundary snapping near detectable window edges.

### Window mode

- highlight the window under the pointer;
- click to capture it;
- Escape cancels;
- Space switches back to Area mode.

Window capture should use the underlying target window, not crop from the visible desktop when a direct window capture path is available.

Preferred native API:

- `IGraphicsCaptureItemInterop::CreateForWindow`

Window capture belongs in the first complete screenshot release, not in a distant future phase.

---

# 7.4 Screenshot Selection Overlay

The overlay should resemble the behavior of Snipping Tool rather than a conventional app window.

Requirements:

- borderless;
- covers the target monitor;
- correct per-monitor DPI behavior;
- selection rectangle tracks pointer precisely;
- non-selected area is dimmed;
- selected area remains undimmed;
- live selection dimensions;
- window highlighting in Window mode;
- Escape cancels;
- no visible titlebar;
- overlay itself must not contaminate the captured content.

Preferred implementation:

- freeze/capture the target monitor before drawing the selection overlay;
- draw the frozen image as the overlay background;
- crop the frozen source frame after selection.

This gives a "frozen desktop" interaction and eliminates overlay contamination.

If a different capture order is chosen, acceptance tests must prove that the overlay is never present in final output.

---

# 7.5 Unified Capture Bar

Provide a small transient Capture Bar inspired by the simplicity of modern capture utilities.

Minimum structure:

```text
┌──────────────────────────────────────────┐
│  Screenshot    Record                   │
│  Area      Window      Display           │
└──────────────────────────────────────────┘
```

Requirements:

- opens from one configurable master hotkey;
- disappears once capture target selection begins;
- keyboard navigable;
- remembers the user's last-used mode where appropriate;
- never expands into a large persistent application window.

Direct hotkeys should remain available for users who never want to see the Capture Bar.

---

# 7.6 Post-Capture Thumbnail

After a screenshot completes, show a small transient thumbnail near a screen corner.

The thumbnail is supplemental. The screenshot must already be usable through the clipboard.

Required behaviors:

- appears only after or concurrently with clipboard completion;
- dismisses automatically after a configurable short duration;
- click opens a lightweight preview or the saved image;
- drag begins a normal Windows drag-and-drop operation;
- user can drag the capture directly into applications that accept files/images;
- thumbnail itself must be excluded from future screenshots/recordings where supported;
- Escape or explicit close dismisses it.

The thumbnail must not block the user's next capture.

The thumbnail is **not** an excuse to build an annotation editor in MVP.

---

# 7.7 Video Recording Flow

Expected flow:

1. User invokes Record from the Capture Bar or direct hotkey.
2. User selects an Area for MVP recording.
3. App dismisses the selection overlay.
4. A brief countdown is optional and configurable.
5. Recording begins.
6. A small always-on-top recording control appears.
7. During recording, user can:
   - Pause;
   - Resume;
   - Restart from the beginning;
   - Discard;
   - Stop.
8. User can also stop/pause with configured global hotkeys.
9. Video is finalized to MP4 on Stop.
10. Optional notification provides the file location.

Pause semantics:

> Paused wall-clock time must be excluded from the final recording timeline. Resume continues the output timeline without an empty or frozen gap.

Restart semantics:

> Restart discards the current in-progress recording and begins again with the same target/settings without requiring target reselection unless capture state became invalid.

Discard semantics:

> Discard stops capture, deletes temporary recording artifacts, and produces no final output file.

MVP recording does not need audio.

---

# 8. Functional Requirements

# 8.1 Capture Targets

Screenshot MVP:

- rectangular Area within one monitor;
- application Window;
- entire Display.

Recording MVP:

- rectangular Area within one monitor.

Post-MVP recording targets:

- application Window;
- entire Display;
- selection spanning multiple monitors.

Cross-monitor Area capture is explicitly deferred until single-monitor color correctness is complete.

For screenshot targeting, Area / Window / Display should share the same Capture Bar rather than behaving as unrelated tools.

---

# 8.2 Monitor Enumeration

The platform layer must enumerate displays and retain sufficient information to:

- map screen coordinates to the correct physical monitor;
- obtain `HMONITOR`;
- identify display path / target information;
- determine Advanced Color / HDR state;
- query current SDR white level;
- determine monitor bounds;
- determine DPI scaling;
- create a capture item for the monitor.

---

# 8.3 HDR / Advanced Color Detection

The display subsystem must detect whether the selected display is operating in HDR / Advanced Color mode.

Do not infer HDR solely from monitor model or EDID capability.

The relevant question is:

> Is the Windows desktop for this display currently using Advanced Color / HDR behavior requiring HDR-aware capture and SDR conversion?

Advanced Color enabled is not the same as HDR enabled. An Advanced Color SDR display and an HDR display need different SDR-white handling; the descriptor must report both states.

Store this in a per-monitor descriptor returned to the UI and capture subsystems.

---

# 8.4 SDR White Level

For HDR-enabled displays, query the current Windows SDR white level using Display Configuration APIs.

Use:

- `DisplayConfigGetDeviceInfo`
- `DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL`
- `DISPLAYCONFIG_SDR_WHITE_LEVEL`

Windows exposes `SDRWhiteLevel` as a value relative to the SDR reference level.

The capture/color pipeline must account for this value when translating ordinary SDR desktop UI from the HDR desktop representation into final SDR output.

This requirement is critical for light-theme correctness.

Do not hard-code a single assumed SDR-white brightness.

---

# 8.5 Windows Capture API

Preferred capture API:

- `Windows.Graphics.Capture`

Create monitor capture targets through:

- `IGraphicsCaptureItemInterop::CreateForMonitor`

Post-MVP window capture can use:

- `IGraphicsCaptureItemInterop::CreateForWindow`

The capture path must use a pixel representation that preserves HDR/WCG information where required.

Preferred format for HDR/Advanced Color desktop capture:

- `DXGI_FORMAT_R16G16B16A16_FLOAT`

Do not collapse the source into an 8-bit UNORM texture before color conversion.

---

# 8.6 D3D11

Use Direct3D 11 for the initial implementation.

Responsibilities:

- device creation;
- capture-frame texture handling;
- crop operations;
- shader-based color conversion;
- scaling where necessary;
- SDR output texture creation;
- video pixel-format conversion;
- GPU-side copies.

Prefer keeping frames on the GPU until readback/encoding requires otherwise.

Avoid CPU pixel-by-pixel tone mapping.

---

# 9. Color Pipeline

# 9.1 Color Correctness Contract

For ordinary SDR application content displayed on an HDR-enabled Windows monitor, final output should visually match the same application captured with HDR disabled, within reasonable colorimetric/tone-mapping tolerance.

Critical content:

- `#FFFFFF` browser/IDE backgrounds;
- light neutral greys;
- dark text;
- antialiased text edges;
- code syntax colors;
- saturated UI accents;
- dark UI;
- mixed SDR UI + HDR media.

The app must not merely make the image "less washed out." It must have an explicit, testable color transform.

---

# 9.2 Capture Representation

When HDR/Advanced Color is enabled, capture into FP16/scRGB-capable surfaces.

Preferred source texture:

`R16G16B16A16_FLOAT`

This retains values outside the ordinary 0-1 SDR range.

---

# 9.3 Tone Mapping / SDR Mapping

Implement a GPU shader responsible for converting source desktop pixels to target SDR.

The implementation must distinguish:

- ordinary SDR UI represented inside the HDR desktop;
- extended highlights / HDR content;
- target SDR reference white;
- out-of-range values requiring compression.

The default mode should be:

**Match Windows desktop / SDR UI**

This mode should prioritize faithful reproduction of normal desktop UI rather than cinematic highlight preservation.

Optional future tone-mapping modes:

- BT.2390-style mapping;
- Reinhard;
- ACES-inspired mapping.

Do not expose multiple algorithms until the default pipeline passes acceptance tests.

---

# 9.4 Target Screenshot Color Space

MVP screenshot output:

- 8-bit per channel;
- sRGB transfer/color behavior;
- PNG;
- correct color metadata where appropriate.

The resulting image should appear correct in:

- Windows Photos;
- Chrome/Edge;
- GitHub;
- Slack;
- Discord;
- VS Code Markdown preview;
- common SDR monitors.

---

# 9.5 SDR Monitor Behavior

If the source monitor is SDR:

- do not run destructive HDR tone mapping;
- use the normal SDR capture path;
- ensure output remains faithful.

The tool must not make SDR capture worse.

---

# 9.6 Mixed SDR + HDR Content

A test page/window should contain:

- normal white UI;
- normal greys/text;
- saturated SDR colors;
- HDR image or HDR video content.

Expected behavior:

- normal UI remains visually faithful;
- HDR highlights compress into SDR without clipping the entire image;
- no global grey veil;
- no severe desaturation.

---

# 10. Screenshot Pipeline

Recommended pipeline:

```text
Global hotkey
    ↓
Resolve monitor
    ↓
Read monitor HDR + SDR-white state
    ↓
Create/reuse Windows.Graphics.Capture session
    ↓
Acquire FP16 frame
    ↓
Freeze source texture
    ↓
Present selection overlay
    ↓
Receive region bounds
    ↓
Crop GPU texture
    ↓
HDR/WCG → SDR shader
    ↓
8-bit sRGB staging/output texture
    ↓
PNG encode
    ↓
Clipboard + optional file
```

Implementation should minimize extra GPU→CPU→GPU round-trips.

---

# 11. Clipboard Requirements

After successful screenshot capture:

- copy the image to the Windows clipboard automatically;
- support standard image paste into common apps;
- preserve correct dimensions;
- preserve alpha only where useful/valid;
- do not require saving a file first.

Where practical, provide:

- a PNG representation;
- a bitmap representation compatible with Windows applications.

Clipboard handling must be resilient if another application temporarily owns or locks the clipboard.

Retry briefly on expected clipboard contention.

Do not hang the UI indefinitely.

---

# 12. File Output

Default screenshot format:

- PNG

Optional later:

- JPEG
- WebP
- AVIF
- HDR-capable image formats

Default screenshot behavior:

- copy to clipboard;
- optionally save automatically.

Default recording format:

- `.mp4`

File naming example:

`Capture 2026-09-22 13-42-18.png`

`Recording 2026-09-22 13-45-03.mp4`

User-configurable output folder.

---

# 13. Video Pipeline

# 13.1 MVP Video Contract

Input:

- one monitor;
- one rectangular region;
- optional cursor;
- no audio.

Output:

- SDR;
- MP4;
- H.264;
- Rec.709;
- hardware acceleration where possible.

Target frame rates:

- 30 FPS default;
- 60 FPS selectable.

---

# 13.2 Recommended Pipeline

```text
Windows.Graphics.Capture
    ↓
FP16 D3D11 texture
    ↓
crop
    ↓
HDR/WCG → SDR shader
    ↓
Rec.709 conversion
    ↓
NV12
    ↓
Media Foundation
    ↓
hardware H.264 encoder
    ↓
MP4 muxer
```

The video pipeline should remain GPU-oriented.

Avoid reading full-resolution frames back to system memory per frame unless no supported hardware path exists.

---

# 13.3 Timestamps

Use capture-frame timing information supplied by Windows where available.

Recording must:

- maintain monotonic timestamps;
- handle duplicate or delayed frames gracefully;
- avoid gradually drifting playback duration;
- finalize playable video after normal stop.

---

# 13.4 Encoder Selection

MVP:

- H.264 hardware encoder through Media Foundation where available.

Fallback:

- supported software encoder if hardware initialization fails.

Post-MVP:

- AV1
- HEVC
- HDR video export

H.264 is the default because interoperability is more important than codec novelty.

---

# 13.5 Region Handling

The recording source may capture the full target monitor and crop to the selected rectangle on the GPU.

This is acceptable for MVP.

The recorder must not include:

- recording controls;
- selection overlay;
- Capture Bar;
- countdown UI;
- post-capture thumbnails;
- unexpected app-owned capture UI inside the selected region.

---

# 13.6 Excluding Application-Owned UI from Capture

Application-owned capture UI should be excluded from screen capture at the window level wherever Windows supports it.

Preferred mechanism for eligible top-level windows:

- `SetWindowDisplayAffinity`
- `WDA_EXCLUDEFROMCAPTURE`

Use this for UI such as:

- Capture Bar;
- recording controls;
- countdown windows;
- post-capture thumbnail;
- capture notifications when appropriate.

Requirements:

- check the return value;
- capture `GetLastError()` on failure;
- verify exclusion behavior in integration/manual tests;
- do not treat the API as a security boundary;
- do not assume exclusion works in every composition/capture edge case.

Fallback behavior if reliable exclusion cannot be established for a given window:

1. hide the app-owned window before capture, or
2. move it outside the captured region where practical, or
3. use the frozen-frame screenshot workflow so app UI cannot contaminate the source.

The product invariant is:

> Our capture UI must not appear in the user's final screenshot or recording.

---

# 14. Audio

Audio is explicitly deferred from MVP.

Future scope:

- system audio via WASAPI loopback;
- microphone input;
- independent enable/disable controls;
- AAC encoding;
- audio/video synchronization.

Do not delay screenshot/video MVP to implement audio.

---

# 15. Cursor

Screenshot:

- default: include cursor = false.

Recording:

- default: include cursor = true.

Expose user preference.

Use `GraphicsCaptureSession.IsCursorCaptureEnabled` where appropriate.

Do not manually composite a second cursor unless required to meet capture behavior.

---

# 16. Recording Indicator and Controls

While recording, show a small control surface with:

- elapsed output duration;
- Pause / Resume;
- Stop;
- Restart;
- Discard.

All four actions are in view, each with its hotkey. (Decided with the
owner in Milestone 4: an overflow menu for two actions only slowed a
practised user down.)

Example:

```text
┌──────────────────────────────────────────────────────────────────────────┐
│ ● 01:24   Pause Ctrl+Alt+P   Stop Ctrl+Alt+R   Restart Ctrl+Alt+N   Discard Ctrl+Alt+D │
└──────────────────────────────────────────────────────────────────────────┘
```

Requirements:

- always accessible;
- keyboard-accessible: global hotkeys for every action, registered only
  while recording, work from anywhere; the control never takes the
  keyboard from the app being recorded by itself, so typing is never
  mistaken for a command, and once clicked it answers single letters;
- excluded from the capture through `WDA_EXCLUDEFROMCAPTURE` where reliable;
- fallback to positioning outside the region or hiding when exclusion cannot be guaranteed;
- stop and pause hotkeys work even if the control is hidden;
- pause removes elapsed paused time from the encoded timeline;
- restart reuses the current target/settings;
- discard leaves no completed recording behind;
- Discard and Restart never lose a take by accident: by default they ask
  first (the recording pauses while asking); with asking turned off they
  act at once and can be undone for 10 seconds (a discarded take is held
  paused; the take a restart replaced is kept as its own file if undone).

Do not place a heavy application window on screen during recording.

---

# 16.1 Optional Interaction Metadata

Design the recorder so it can optionally persist lightweight pointer interaction metadata alongside a recording.

Allowed event types:

- pointer movement samples;
- left/right/middle mouse button down/up;
- click location;
- timestamp relative to the recording timeline.

Example:

```json
{
  "version": 1,
  "events": [
    { "t": 1.242, "type": "move", "x": 921, "y": 544 },
    { "t": 1.481, "type": "click", "button": "left", "x": 921, "y": 544 }
  ]
}
```

Purpose:

- future cursor smoothing;
- future cursor resizing/restyling;
- future click rings;
- future automatic zoom around clicks;
- optional cursor removal/re-rendering in a future editor.

Privacy rules:

- never record typed text;
- never record key values;
- do not implement keystroke logging;
- do not record clipboard contents;
- interaction metadata is local only;
- metadata capture must be disableable.

This feature is architecture-enabling and must not delay screenshot correctness or basic recording.

---

# 17. UI Responsibilities (GPUI Kit)

The GPUI Kit application should own:

- settings window;
- tray-driven UI surfaces where applicable;
- selection overlay presentation;
- transient capture toolbar;
- recording controls;
- notifications/toasts;
- preference forms;
- optional preview UI;
- post-capture thumbnail;
- thumbnail drag gesture (the platform layer performs the Windows drag-and-drop);
- Area/Window mode presentation and live dimension display.

UI code should not own:

- WinRT capture frame processing;
- D3D11 resource management;
- tone-mapping shaders;
- Media Foundation encoder internals;
- Windows clipboard internals;
- DisplayConfig queries.

Every GPUI window's `HWND` is available through `HasWindowHandle`. Window behavior GPUI does not expose directly (topmost overlay styles, capture exclusion, hiding a window without destroying it) is applied to that handle by the platform layer.

UI code never blocks the GPUI main thread on capture, encoding, clipboard, or file work. It runs those calls on GPUI's background executor and updates views with the result.

---

# 18. Platform and Capture Responsibilities

The platform and capture crates should own:

- global hotkeys;
- tray icon;
- monitor enumeration;
- display-state discovery;
- HDR / Advanced Color state;
- SDR white-level queries;
- DPI/coordinate conversion helpers;
- `Windows.Graphics.Capture`;
- capture-item creation;
- D3D11 device/context;
- GPU textures;
- crop/scale shaders;
- HDR→SDR conversion;
- screenshot encoding;
- clipboard;
- video encoder/muxer;
- application-owned window capture exclusion;
- Windows drag-and-drop/file data integration;
- optional pointer/click interaction metadata capture;
- low-level error mapping.

---

# 19. UI ↔ Capture API Boundary

Keep the public API intentionally small.

Illustrative Rust API:

```rust
pub struct MonitorInfo {
    pub id: MonitorId,
    pub name: String,
    /// Physical pixels, in virtual-desktop coordinates.
    pub bounds: PhysicalRect,
    pub scale_factor: f32,
    pub advanced_color_enabled: bool,
    pub hdr_enabled: bool,
    pub sdr_white_level_nits: Option<f32>,
}

pub struct Region {
    pub monitor: MonitorId,
    /// Physical pixels, relative to the monitor.
    pub rect: PhysicalRect,
}

pub enum CaptureTarget {
    Region(Region),
    Window(WindowTarget),
    Display(MonitorId),
}

pub struct ScreenshotOptions {
    pub target: CaptureTarget,
    pub include_cursor: bool,
    pub copy_to_clipboard: bool,
    pub save_to_file: bool,
}

pub enum FrameRate {
    Fps30,
    Fps60,
}

pub enum VideoCodec {
    H264,
}

pub struct RecordingOptions {
    pub region: Region,
    pub include_cursor: bool,
    pub frame_rate: FrameRate,
    pub codec: VideoCodec,
    pub record_interaction_metadata: bool,
}

impl Capture {
    pub fn list_monitors(&self) -> Result<Vec<MonitorInfo>, CaptureError>;
    pub fn freeze_monitor(&self, monitor: MonitorId) -> Result<FrozenFrame, CaptureError>;
    pub fn screenshot(
        &self,
        frame: &FrozenFrame,
        options: ScreenshotOptions,
    ) -> Result<ScreenshotResult, CaptureError>;
    pub fn start_recording(&self, options: RecordingOptions) -> Result<Recording, CaptureError>;
}

impl FrozenFrame {
    /// SDR image of the frozen monitor, for the selection overlay.
    pub fn preview(&self) -> Arc<RenderImage>;
}

impl Recording {
    pub fn pause(&self) -> Result<(), CaptureError>;
    pub fn resume(&self) -> Result<(), CaptureError>;
    pub fn restart(&self) -> Result<(), CaptureError>;
    pub fn stop(self) -> Result<RecordingResult, CaptureError>;
    pub fn discard(self) -> Result<(), CaptureError>;
}
```

These calls may block. UI code runs them on GPUI's background executor.

A frozen frame is an owned value holding the FP16 source texture. UI code receives only its SDR preview image, never the source pixels.

Do not expose COM pointers, D3D resources, or native texture handles to UI code unless there is a deliberate rendering integration.

---

# 20. Coordinate Systems and DPI

Windows per-monitor DPI scaling must be handled correctly.

The application must have Per-Monitor V2 DPI awareness.

All APIs should explicitly document whether coordinates are:

- physical pixels;
- logical/DIP units;
- GPU texture coordinates.

Recommended internal rule:

> Capture regions are expressed in physical source pixels.

GPUI reports window geometry in logical pixels. Selection coordinates must be converted to physical pixels, using the window's scale factor, at the UI/capture boundary.

Acceptance testing must cover:

- 100%;
- 125%;
- 150%;
- 175%;
- 200% scaling.

---

# 21. Multi-Monitor Strategy

## MVP

A capture region belongs to exactly one physical monitor.

If drag approaches/leaves target monitor:

- clamp selection to the target monitor, or
- cancel/restart on the new monitor.

Prefer the simplest behavior that is intuitive.

## Post-MVP

Support region capture spanning monitors.

When implementing cross-monitor capture, account for:

- HDR monitor + SDR monitor;
- two HDR monitors with different SDR white settings;
- different DPI scales;
- different resolutions;
- negative virtual desktop coordinates.

Each display may require independent capture/color processing before compositing.

Do not treat the entire virtual desktop as one homogeneous color surface.

---

# 22. Performance Requirements

# 22.1 Background

When idle:

- near-zero GPU utilization;
- negligible CPU usage;
- no continuous screen capture;
- no continuous high-frequency polling.

# 22.2 Screenshot

Target:

- overlay visible within approximately 150 ms of hotkey under normal conditions;
- capture available to clipboard within approximately 300 ms after region release for common 1440p/4K regions.

These are targets, not hard realtime guarantees.

Do not add noticeable multi-second encoding latency.

# 22.3 Video

At 4K source desktop:

- sustain 60 FPS capture/processing on modern hardware when hardware encoding is available;
- keep per-frame work off the GPUI main thread; frames never pass through UI code;
- avoid unbounded frame queues.

If encoder cannot keep up:

- drop frames in a controlled way;
- preserve timestamps;
- surface diagnostics.

---

# 23. Memory Requirements

Do not retain unnecessary historical frames.

For screenshots:

- one or a small number of capture textures;
- release frozen-frame handles promptly.

For video:

- bounded texture/frame pool;
- bounded encoder queue;
- no unbounded accumulation under encoder pressure.

At 4K FP16 RGBA, frame memory is substantial; pool sizes must be deliberate.

---

# 24. Error Handling

Expose human-readable errors for:

- unsupported capture API;
- D3D11 initialization failure;
- capture access failure;
- target monitor disappearing;
- device removed/reset;
- encoder unavailable;
- file write failure;
- clipboard failure;
- invalid region;
- zero-sized region;
- GPU shader failure;
- output directory unavailable.

Capture errors should include:

- stable error code;
- user-facing message;
- optional technical details for logs.

Example:

```rust
pub struct CaptureError {
    pub code: CaptureErrorCode,
    /// Shown to the user.
    pub message: String,
    /// For logs only.
    pub detail: Option<String>,
}

pub enum CaptureErrorCode {
    CaptureUnavailable,
    DeviceLost,
    EncoderUnavailable,
    ClipboardFailed,
    FileWriteFailed,
    InvalidRegion,
}
```

---

# 25. Device Loss and Display Changes

Handle:

- display unplugged;
- HDR toggled while app is running;
- resolution changed;
- DPI changed;
- GPU driver reset;
- sleep/wake;
- docking/undocking.

Do not assume monitor state captured at startup remains valid forever.

At minimum, refresh monitor metadata when a capture begins.

---

# 26. Settings

MVP settings:

## General

- launch at startup;
- screenshot hotkey;
- recording hotkey;
- output directory;
- copy screenshots to clipboard;
- auto-save screenshots.

## Screenshot

- include cursor;
- show notification after capture;
- show post-capture thumbnail;
- thumbnail display duration;
- boundary snapping;
- auto-save behavior.

## Recording

- include cursor;
- 30 / 60 FPS;
- show countdown;
- output directory;
- optional pointer/click metadata capture.

## Advanced / Diagnostics

- detected monitors;
- HDR / Advanced Color state;
- SDR white level;
- active GPU adapter;
- encoder selected.

Do not expose tone-mapping knobs in the normal settings UI until the default algorithm is validated.

---

# 27. Privacy and Security

This is a screen-capture application and therefore handles highly sensitive visual information.

Requirements:

- all processing local by default;
- no automatic uploads;
- no analytics/telemetry by default;
- no capture data transmitted over the network;
- no retained screenshot database unless user enables one in future;
- temporary GPU/CPU buffers released promptly;
- logs must not contain captured pixels.

If an updater is implemented:

- signed releases;
- TLS;
- explicit update metadata;
- no arbitrary remote code execution path.

---

# 28. Logging

Provide local diagnostic logs containing:

- app version;
- Windows version/build;
- GPU adapter name;
- monitor metadata excluding personally identifying monitor labels where unnecessary;
- HDR state;
- SDR white level;
- chosen pixel formats;
- capture initialization success/failure;
- selected encoder;
- dropped-frame counts;
- errors.

Never log:

- image pixels;
- video frames;
- clipboard content;
- window titles by default.

---

# 29. Telemetry

MVP: no remote telemetry.

If telemetry is added later:

- opt-in;
- aggregate only;
- never include capture contents;
- never include application/window titles without explicit consent.

---

# 30. Suggested Repository Structure

```text
/
├─ Cargo.toml              (workspace)
├─ mise.toml
│
├─ crates/
│  ├─ app/                  (the GPUI Kit binary)
│  │  ├─ src/
│  │  │  ├─ main.rs
│  │  │  ├─ capture_bar.rs
│  │  │  ├─ selection_overlay.rs
│  │  │  ├─ recording_controls.rs
│  │  │  ├─ thumbnail.rs
│  │  │  └─ settings.rs
│  │  └─ tests/
│  │     └─ ui.rs
│  │
│  ├─ capture-core/
│  │  ├─ src/
│  │  │  ├─ lib.rs
│  │  │  ├─ monitor.rs
│  │  │  ├─ capture.rs
│  │  │  ├─ frame_pool.rs
│  │  │  └─ errors.rs
│  │
│  ├─ color/
│  │  ├─ src/
│  │  │  ├─ lib.rs
│  │  │  ├─ hdr.rs
│  │  │  ├─ sdr_white.rs
│  │  │  └─ shaders.rs
│  │  └─ shaders/
│  │     ├─ hdr_to_sdr.hlsl
│  │     ├─ crop.hlsl
│  │     └─ rgb_to_nv12.hlsl
│  │
│  ├─ screenshot/
│  │  ├─ src/
│  │  │  ├─ lib.rs
│  │  │  ├─ png.rs
│  │  │  └─ clipboard.rs
│  │
│  ├─ recorder/
│  │  ├─ src/
│  │  │  ├─ lib.rs
│  │  │  ├─ media_foundation.rs
│  │  │  ├─ encoder.rs
│  │  │  └─ mux.rs
│  │
│  └─ windows-platform/
│     ├─ src/
│     │  ├─ lib.rs
│     │  ├─ display_config.rs
│     │  ├─ hotkeys.rs
│     │  ├─ tray.rs
│     │  ├─ window.rs
│     │  ├─ dpi.rs
│     │  ├─ capture_exclusion.rs
│     │  ├─ drag_drop.rs
│     │  ├─ input_metadata.rs
│     │  └─ startup.rs
│
├─ docs/
│  ├─ PRD.md
│  ├─ COLOR_PIPELINE.md
│  └─ TEST_MATRIX.md
│
└─ README.md
```

Exact crate boundaries may be simplified if the implementation remains clean.

---

# 31. Implementation Milestones

# Milestone 0 — Technical Spike: Prove Color Correctness

**Goal:** prove the capture/color path before building product UI.

Build a small command-line or minimal-window executable that:

1. enumerates monitors;
2. selects one monitor;
3. reports:
   - monitor bounds;
   - HDR/Advanced Color state;
   - SDR white level;
4. captures one frame through Windows.Graphics.Capture;
5. obtains HDR-capable FP16 data where appropriate;
6. runs HDR→SDR conversion;
7. writes a PNG.

Test with:

- light-theme browser;
- light-theme IDE;
- dark-theme IDE;
- saturated test colors;
- HDR image/video beside SDR UI.

**Exit criteria:**

The SDR PNG generated while Windows HDR is enabled closely matches a reference screenshot made with Windows HDR disabled for ordinary SDR UI.

Do not proceed to the full app if this criterion is not met.

---

# Milestone 1 — Area Screenshot MVP

Implement:

- GPUI Kit background app with no window until invoked;
- global direct screenshot hotkey;
- monitor selection;
- frozen-screen selection overlay;
- rectangular Area selection;
- live physical-pixel dimensions;
- screenshot crop;
- HDR→SDR conversion;
- PNG encoding;
- immediate clipboard copy;
- Escape cancel.

**Exit criteria:**

A user can perform:

> hotkey → drag → release → paste into browser/chat

with visually correct SDR output.

---

# Milestone 2 — Complete Screenshot Workflow

Implement:

- unified Capture Bar;
- Screenshot / Record mode switch;
- Area / Window / Display targeting;
- Space toggles Area ↔ Window during screenshot targeting;
- `CreateForWindow` window capture;
- whole-display screenshot;
- optional window-boundary snapping;
- transient post-capture thumbnail;
- click thumbnail to open lightweight preview/file;
- drag thumbnail directly into compatible applications;
- output directory;
- auto-save option;
- notifications;
- tray icon;
- settings;
- hotkey configuration;
- app-owned UI capture exclusion;
- monitor changes;
- DPI correctness;
- device-lost handling;
- SDR-monitor path;
- robust clipboard behavior.

**Exit criteria:**

Screenshot workflow feels like a credible day-to-day Snipping Tool replacement, not merely a technical capture demo.

---

# Milestone 3 — Video Technical Spike

Build a minimal recorder without polished UI:

1. capture one monitor;
2. crop a fixed region;
3. run HDR→SDR conversion per frame;
4. convert to NV12;
5. encode H.264 through Media Foundation;
6. write MP4;
7. pause/resume with correct timeline timestamps;
8. stop cleanly.

**Exit criteria:**

A 60-second recording:

- plays correctly;
- has correct duration;
- has stable colors;
- shows no white-wash issue;
- can pause/resume without a timeline gap;
- does not leak memory;
- does not accumulate an unbounded frame queue.

---

# Milestone 4 — Area Recording MVP

Integrate recording into the GPUI Kit app:

- Area selection;
- record start;
- elapsed output timer;
- Pause / Resume;
- Stop;
- Restart;
- Discard;
- stop/pause hotkeys;
- recording-control capture exclusion;
- cursor inclusion;
- 30/60 FPS option;
- output file notification;
- optional lightweight pointer/click metadata if it does not threaten milestone schedule.

**Exit criteria:**

The user can record, pause, resume, restart, discard, and stop without the application's controls appearing in the output.

---

# Milestone 5 — Hardening

Test and fix:

- 4K;
- 4K 120/144 Hz desktop;
- DPI scales;
- sleep/wake;
- HDR toggle;
- monitor disconnect/reconnect;
- multiple monitors;
- GPU device reset;
- encoder fallback;
- long recording;
- unusual selection sizes;
- very small regions;
- high-motion content;
- Capture Bar/thumbnail/recording-control exclusion;
- rapid consecutive screenshots;
- clipboard contention;
- drag-and-drop into common development/chat applications.

---

# Milestone 6 — Future: Presentation Mode

Possible additions inspired by presentation-focused capture tools:

- cursor smoothing;
- enlarged/restyled cursor;
- click highlight/ripple;
- automatic zoom around clicks;
- optional keystroke overlay with explicit privacy design;
- canvas/background/padding;
- quick trim;
- privacy blur/pixelation;
- captions;
- dead-air removal;
- speed changes;
- editable recording projects.

These features are intentionally separated from the capture MVP.

Do not begin them until core SDR screenshot/video correctness and fast capture UX are stable.

---

# Milestone 7 — Other Post-MVP Capture Features

Possible additions:

- Window recording;
- Display recording;
- system audio;
- microphone;
- AV1;
- HEVC;
- cross-monitor Areas;
- true HDR image export;
- HDR10 video export;
- richer screenshot annotation;
- capture history.

---

# 32. Test Plan

# 32.1 Color Reference Test

Create a deterministic desktop test surface containing:

- pure white: `#FFFFFF`;
- near-white: `#F7F7F7`;
- neutral greys;
- pure black;
- dark text on white;
- subpixel/antialiased text;
- red/green/blue patches;
- saturated CSS colors;
- gradients;
- code editor syntax colors;
- HDR test image/video.

Capture the same surface:

A. Windows HDR disabled.  
B. Windows HDR enabled using the application.

Compare A and B for SDR portions.

Tests should include both visual review and image metrics.

Possible metrics:

- per-pixel difference for known SDR test regions;
- ΔE-based color comparison where practical;
- luminance comparison for white/grey patches.

Do not require HDR highlight regions to match the SDR-disabled reference exactly.

---

# 32.2 SDR White-Level Matrix

Test multiple Windows "SDR content brightness" settings on the HDR monitor.

The same SDR application content should remain stable in final SDR capture even though Windows internally changes how SDR white is represented on the HDR desktop.

This is a key acceptance test.

---

# 32.3 Display Matrix

Minimum manual matrix:

- HDR off, 100% scaling;
- HDR on, low SDR brightness;
- HDR on, medium SDR brightness;
- HDR on, high SDR brightness;
- 125% scaling;
- 150% scaling;
- 200% scaling;
- secondary SDR monitor;
- secondary HDR monitor if available.

---

# 32.4 Application Matrix

Test screenshots and video using:

- VS Code light theme;
- VS Code dark theme;
- Windows Terminal;
- Chrome/Edge light webpages;
- GitHub;
- Notepad;
- File Explorer;
- a GPUI application such as Zed;
- image viewer;
- HDR YouTube/video playback beside SDR UI.

---

# 32.5 Destination Matrix

Paste/open screenshots in:

- Slack or equivalent chat app;
- Discord;
- GitHub issue/PR;
- Chrome;
- Edge;
- VS Code;
- Windows Photos.

Play recordings in:

- Windows Media Player;
- Chrome;
- Edge;
- common chat/browser upload destination.

---

# 32.6 Capture UX Matrix

Verify:

- direct screenshot hotkey;
- Capture Bar screenshot mode;
- Capture Bar record mode;
- Area selection;
- Space toggles Area ↔ Window;
- window hover highlighting;
- Display screenshot;
- live Area dimensions;
- boundary snapping enabled/disabled;
- immediate clipboard availability;
- transient thumbnail appearance/dismissal;
- thumbnail drag into Explorer;
- thumbnail drag into at least two common chat/browser upload targets;
- own UI exclusion from screenshots;
- own UI exclusion from video;
- pause/resume;
- restart;
- discard;
- stop hotkey while recording control is hidden.

---

# 33. Automated Tests

Automate wherever practical:

## Unit Tests

- SDR white-level conversion math;
- monitor-coordinate conversion;
- DPI conversion;
- crop bounds;
- filename generation;
- timestamp generation;
- encoder configuration;
- error mapping.

## GPU/Golden Tests

Where stable:

- feed known FP16/scRGB samples into tone-mapping shader;
- compare output against golden values;
- test SDR white;
- test HDR highlights;
- test negative scRGB values if present;
- test values > 1.0.

## Integration Tests

- create capture session;
- receive frame;
- save screenshot;
- confirm dimensions;
- start/stop recording;
- pause/resume recording and verify timeline duration excludes pause;
- restart recording;
- discard recording and verify no final output remains;
- verify app-owned top-level capture UI is excluded where supported;
- confirm MP4 exists and duration is sane.

## UI Tests

Drive the real views in headless GPUI windows with `#[gpui_kit::test]` and `gpui_kit::test`, with capture behind a fake implementation of the capture API:

- Capture Bar keyboard navigation and mode memory;
- selection overlay drag geometry and the displayed physical-pixel dimensions at several scale factors;
- Space toggling Area ↔ Window;
- Escape cancelling selection;
- recording controls through pause, resume, restart, discard, and stop;
- settings forms persisting their values;
- error states rendering the user-facing message.

Real display/color validation will still require hardware/manual testing.

---

# 34. Acceptance Criteria

# 34.1 Screenshot Core

A build is not acceptable unless all are true:

- [ ] Windows 11 supported.
- [ ] Global direct screenshot hotkey works.
- [ ] Unified Capture Bar works.
- [ ] Single-monitor Area selection works.
- [ ] Window screenshot selection works.
- [ ] Display screenshot selection works.
- [ ] Space toggles Area ↔ Window while screenshot targeting.
- [ ] Escape cancels.
- [ ] Area selection shows live physical-pixel dimensions.
- [ ] Selection overlay is absent from final capture.
- [ ] App-owned capture UI is absent from final capture.
- [ ] HDR monitor capture does not collapse source to 8-bit before tone mapping.
- [ ] Current SDR white level is queried.
- [ ] HDR→SDR conversion runs for HDR/Advanced Color display capture.
- [ ] SDR monitors are captured without destructive tone mapping.
- [ ] PNG output is standard SDR/sRGB-compatible.
- [ ] Screenshot is copied to clipboard immediately after capture.
- [ ] Clipboard availability is not delayed by thumbnail, notification, preview, or auto-save UI.
- [ ] Screenshot can be pasted directly into common applications.
- [ ] Post-capture thumbnail appears when enabled.
- [ ] Thumbnail can be dismissed without affecting the capture.
- [ ] Thumbnail can be dragged into compatible applications.
- [ ] Boundary snapping can be enabled/disabled if implemented.
- [ ] Light-theme white backgrounds are not visibly washed out.
- [ ] Light greys retain usable contrast.
- [ ] Dark text remains correct.
- [ ] Saturated UI colors are not severely desaturated.
- [ ] DPI scaling works at 100%, 150%, and 200%.

# 34.2 Screenshot Color Correctness

For the defined SDR test surface:

- [ ] HDR-enabled app capture is visually equivalent to HDR-disabled reference for ordinary SDR UI.
- [ ] Changing Windows SDR-content brightness does not materially change the final SDR screenshot of the same SDR UI.
- [ ] HDR highlights compress gracefully rather than globally washing out the image.

# 34.3 Capture UI Exclusion

- [ ] Capture Bar is not present in final captures.
- [ ] Selection overlay is not present in final captures.
- [ ] Recording controls are not present in final recordings.
- [ ] Post-capture thumbnail is not accidentally recorded in subsequent captures where exclusion is expected.
- [ ] `WDA_EXCLUDEFROMCAPTURE` results are checked rather than assumed.
- [ ] A tested hide/move/frozen-frame fallback exists when capture exclusion is not reliable.

# 34.4 Video Core

- [ ] Single-monitor rectangular recording works.
- [ ] Output is MP4/H.264.
- [ ] Output is SDR/Rec.709.
- [ ] HDR desktop source is converted before SDR encode.
- [ ] Cursor inclusion setting works.
- [ ] 30 FPS works.
- [ ] 60 FPS works on suitable hardware.
- [ ] Pause works.
- [ ] Resume works.
- [ ] Paused wall-clock time is excluded from final output duration.
- [ ] Restart begins a clean recording with the same target/settings.
- [ ] Discard leaves no completed output file.
- [ ] Stop hotkey works.
- [ ] Finalized file plays normally.
- [ ] Duration is accurate.
- [ ] No unbounded frame queue.
- [ ] No obvious memory growth over a 30-minute recording.
- [ ] Light-theme content is not washed out.
- [ ] Recording controls are excluded from output.

# 34.5 Optional Interaction Metadata

If pointer/click metadata is implemented:

- [ ] timestamps use the recording output timeline, excluding paused time;
- [ ] coordinates map correctly to the captured region;
- [ ] pointer movement/clicks only;
- [ ] no typed characters are captured;
- [ ] no key values are persisted;
- [ ] no clipboard content is persisted;
- [ ] metadata can be disabled;
- [ ] metadata remains local.

---

# 35. Definition of Done for MVP

The MVP is complete when a Windows 11 user with HDR permanently enabled can use this application instead of Snipping Tool for normal development communication.

The following workflow must be boring and reliable:

## Screenshot

> Press hotkey → drag around a VS Code/browser region → release → paste into Slack/GitHub.

Or:

> Press hotkey → Space → click a window → drag the transient thumbnail into a chat/browser.

The image should already be on the clipboard and should look like the user's desktop.

## Video

> Choose record → drag region → record a short UI interaction → pause/resume if needed → stop → attach MP4.

The video should look correct in ordinary SDR playback without asking recipients to enable HDR or use a special player, and the application's own recording controls must not appear in the output.

That is the product.

---

# 36. Technical Risks

# Risk 1 — Exact Windows HDR Desktop Mapping

The most important technical risk is reproducing the correct mapping of ordinary SDR UI embedded in the HDR desktop.

Mitigation:

- make Milestone 0 mandatory;
- inspect FP16 values directly;
- query Windows SDR white level;
- compare against HDR-disabled reference captures;
- document color math;
- keep shader behavior deterministic.

---

# Risk 2 — GPUI Window Semantics

GPUI does not expose every specialized Windows window behavior required for a Snipping Tool overlay: topmost overlay styles, capture exclusion, hiding a window without destroying it, and focus from a global hotkey. It also has no global hotkeys, tray icon, or outgoing drag-and-drop.

Mitigation:

- use GPUI Kit windows and components for all product UI;
- apply narrow Win32 calls to each window's `HWND`, obtained through `HasWindowHandle`;
- keep those calls in the platform crate;
- measure overlay latency early; if creating a window per capture misses the 150 ms target, pre-create the overlay and show/hide it through its `HWND`.

---

# Risk 3 — GPU Texture Sharing Between Capture and UI

GPUI renders with Direct3D 11 on Windows but does not expose its device or textures. Displaying the frozen capture without a readback would require integration work.

Mitigation:

Initial implementation may:

- perform one GPU→CPU conversion for the frozen selection-overlay preview if necessary;
- keep final screenshot source data in native FP16/GPU form.

Do not compromise final color correctness just to avoid one transient preview copy.

---

# Risk 4 — Media Foundation Hardware Encoder Variance

Hardware encoder availability/behavior can vary by GPU and driver.

Mitigation:

- enumerate encoder transforms;
- prefer hardware H.264;
- support software fallback;
- log selected encoder;
- isolate encoder behind an interface.

---

# Risk 5 — High Refresh Rate vs Recording Rate

The desktop may run at 120/144/165 Hz while recording at 30/60 FPS.

Mitigation:

- recording cadence is independent of display refresh;
- use capture timestamps;
- do not attempt to encode every desktop refresh;
- respect configured FPS.

---

# 37. Architecture Decision Records to Create

The implementing agent should create ADRs for:

1. Why Windows.Graphics.Capture was selected.
2. Why D3D11 was selected over D3D12.
3. Exact HDR/scRGB → SDR transform.
4. Use of Windows SDR white level.
5. Screenshot encoder choice.
6. Media Foundation encoder architecture.
7. UI/capture API boundary.
8. DPI/coordinate conventions.
9. Multi-monitor strategy.
10. Hardware encoder fallback behavior.
11. App-owned capture UI exclusion and fallback strategy.
12. Post-capture thumbnail drag/drop architecture.
13. Optional pointer/click metadata format and privacy boundary.

The color-pipeline ADR is mandatory and should contain equations / shader assumptions.

---

# 38. Agent Implementation Rules

The implementation agent must follow these constraints:

1. Build all product UI with GPUI Kit. Where GPUI lacks a window behavior, apply a narrow Win32 call to the window's `HWND` rather than adding a second UI stack.
2. Do not add Electron, Tauri, a WebView, or a JavaScript runtime.
3. Do not implement cross-platform abstractions.
4. Do not optimize for Windows 10.
5. Do not use GDI/BitBlt as the primary capture path.
6. Do not convert HDR capture to 8-bit before tone mapping.
7. Do not hard-code SDR white to a single brightness value.
8. Do not implement video before screenshot color correctness is proven.
9. Do not implement audio before video color correctness is proven.
10. Do not add network uploads in MVP.
11. Do not add telemetry in MVP.
12. Do not expose native graphics resources directly to UI code unless required.
13. Keep native resource lifetimes explicit and safe.
14. Keep frame queues bounded.
15. Treat light-theme correctness as a release blocker.
16. Clipboard completion must not wait for thumbnail or preview UI.
17. Treat app-owned capture UI appearing in output as a release blocker.
18. Use `WDA_EXCLUDEFROMCAPTURE` only with return/error checks and tested fallbacks.
19. Do not record typed characters or key values as part of interaction metadata.
20. Do not expand Presentation Mode features into MVP.
21. Never block the GPUI main thread on capture, encoding, clipboard, or file work.

---

# 39. Suggested First Agent Task

The first implementation task should not be "scaffold the whole app."

It should be:

> Build `capture-spike`, a Windows 11 Rust executable that enumerates the active monitor, reports HDR/Advanced Color state and SDR white level, captures a frame using Windows.Graphics.Capture into an HDR-capable format, converts that frame to SDR using a documented shader, and writes `capture.png`. Include a README describing the exact color transform and a manual comparison procedure against a screenshot taken with Windows HDR disabled.

Only after `capture-spike` passes the reference test should the agent build the GPUI Kit application.

---

# 40. Reference APIs / Documentation

Implementation should begin from official Microsoft documentation for these APIs:

- Windows.Graphics.Capture
  - https://learn.microsoft.com/windows/uwp/audio-video-camera/screen-capture

- GraphicsCaptureSession
  - https://learn.microsoft.com/uwp/api/windows.graphics.capture.graphicscapturesession

- CreateForMonitor
  - https://learn.microsoft.com/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createformonitor

- CreateForWindow
  - https://learn.microsoft.com/windows/win32/api/windows.graphics.capture.interop/nf-windows-graphics-capture-interop-igraphicscaptureiteminterop-createforwindow

- DirectX with Advanced Color / HDR
  - https://learn.microsoft.com/windows/win32/direct3darticles/high-dynamic-range

- DISPLAYCONFIG_SDR_WHITE_LEVEL
  - https://learn.microsoft.com/windows/win32/api/wingdi/ns-wingdi-displayconfig_sdr_white_level

- DisplayConfigGetDeviceInfo
  - https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-displayconfiggetdeviceinfo

- Media Foundation
  - https://learn.microsoft.com/windows/win32/medfound/microsoft-media-foundation-sdk

- SetWindowDisplayAffinity / WDA_EXCLUDEFROMCAPTURE
  - https://learn.microsoft.com/windows/win32/api/winuser/nf-winuser-setwindowdisplayaffinity

- GPUI Kit
  - https://gpui-kit.com/docs
  - https://gpui-kit.com/llms.txt (index of every page, as Markdown)
  - https://github.com/longbridge/gpui-kit

Prefer official API documentation over copied implementation snippets from third-party screen-capture projects.

Third-party projects may be inspected for design ideas, but their tone-mapping behavior must not be accepted as correct without independent validation.

Product/UX inspiration:

- ScreenFlare screenshots
  - https://screenflare.app/docs/screenshots
- ScreenFlare recording controls
  - https://screenflare.app/docs/recording
- ScreenFlare editing concepts for future Presentation Mode
  - https://screenflare.app/docs/editing

ScreenFlare is product inspiration only. Do not copy platform-specific implementation assumptions from macOS into the Windows architecture.

---

# 41. Open Questions

These are not blockers for Milestone 0:

1. Exact final application name.
2. Default hotkeys.
3. Whether screenshot files should auto-save by default.
4. Whether the overlay preview should be rendered directly from a shared GPU texture.
5. Default post-capture thumbnail duration.
6. Whether boundary snapping is enabled by default.
7. Whether AV1 should be offered after H.264.
8. Whether a lightweight capture history is desirable later.
9. Whether pointer/click interaction metadata should be enabled by default or opt-in.
10. Whether interaction metadata should live in a sidecar file or an app-managed metadata directory.

The agent should not block implementation on these questions.

---

# 42. Product Summary

Build a Windows 11-only capture utility with the interaction speed of Snipping Tool and a deliberately correct HDR-aware capture pipeline.

The defining technical path is:

```text
Windows.Graphics.Capture
        ↓
FP16 / scRGB source
        ↓
per-monitor Windows SDR white-level awareness
        ↓
GPU HDR/WCG → SDR transform
        ↓
sRGB PNG
or
Rec.709 video → H.264 MP4
```

The defining product requirements are simpler:

> A screenshot or recording of a light-themed development UI must look like the screen the user actually saw, even when Windows HDR is enabled.

and:

> Capturing it should feel as immediate as Windows Snipping Tool: one hotkey, fast target selection, instant clipboard output, unobtrusive controls, and no capture-tool UI leaking into the result.
