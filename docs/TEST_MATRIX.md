# Test matrix

Milestone 2 (screenshot workflow) is in progress, steps 1–4 of 6 accepted; see [Milestone
2](#milestone-2-screenshot-workflow). Milestone 1 (area screenshot) **passed** the owner's acceptance test on
2026-09-27; see [Milestone 1](#milestone-1-area-screenshot). The Milestone 0 gate (PRD §31, §32.1, §32.2, §34.2) **passed** on 2026-09-27
(session `gate-20260927-112049`, after the retakes described in [Retaking
captures](#retaking-captures)). The project owner confirmed the
pass the same day, and Milestone 1 began. [COLOR_PIPELINE.md](COLOR_PIPELINE.md) describes
what is being tested.

## Automated checks

From the repository root, in PowerShell:

```powershell
mise exec -- cargo fmt --all -- --check
mise exec -- cargo clippy --all-targets -- -D warnings
mise exec -- cargo test
```

`cargo test` includes GPU golden tests. They run on WARP (software, so they
work anywhere) and on the hardware adapter when there is one.

## Milestone 2: screenshot workflow

Milestone 2 is built in six steps, each tried and approved by the owner: (1)
tray, settings, log file, auto-save, single instance; (2) window and display
capture, Space toggle, snapping; (3) the Capture Bar; (4) thumbnail,
notifications, capture exclusion; (5) the settings window; (6) hardening and
an HDR video test of the highlight curve.

### Step 1: tray, settings, log file, auto-save, single instance

**Passed**: the owner ran all seven acceptance steps below on 2026-09-27.

Automated: settings round trip, defaults for missing fields, a broken file
kept as `settings.json.bad`; file names and ` (2)` suffixes; the tray icon
drawing and `HICON` at tray sizes; the tray menu following the settings;
the single-instance mutex; a second instance reaching the first.

Smoke test (release build, `FRAMECUT_DATA_DIR` pointing at a scratch
folder so the owner's settings are untouched), 2026-09-27: the tray window
exists; the hotkey and a tray click each put a screenshot on the clipboard
and a file in the folder (clipboard 2–3 ms, file 11 ms after release); a
second launch exits with code 0 and the first logs the signal and keeps
running; Escape cancels; the quit hotkey ends the app; the log has no
errors.

The new log file showed two errors GPUI had been logging unseen since
Milestone 1: moving the overlay inside GPUI's `open_window` callback
(`RefCell already borrowed`, so GPUI missed the new bounds), and destroying
the overlay before GPUI handled its deactivation (`window not found`). The
overlay is now placed after `open_window` returns and hidden before it is
removed.

Acceptance (manual):

1. Start it: `mise exec -- cargo run --release -p framecut`. The Framecut
   icon (blue square, white corners) appears in the tray, possibly under the
   `^` overflow; drag it onto the taskbar to keep it visible. On the first
   run a notification says Framecut is running.
2. Press **Ctrl+Alt+S**, drag, release. Paste: the screenshot is there.
3. Right-click the tray icon → **Open screenshots folder**. Explorer opens
   `Pictures\Framecut` with `Capture <date> <time>.png`.
4. Left-click the tray icon: the overlay appears (after a short pause so the
   tray closes first). Drag; a second file appears.
5. Right-click the tray icon → uncheck **Save screenshots to the folder**.
   Take a screenshot: it pastes, but no new file appears. Check it again.
6. Start Framecut a second time from another terminal. It exits at once and
   the running one shows "Framecut is already running".
7. Right-click the tray icon → **Quit Framecut**. The icon disappears.
8. The log is `%LOCALAPPDATA%\Framecut\logs\framecut.log` and settings are
   `%APPDATA%\Framecut\settings.json`.

### Step 2: window and display capture, Space toggle, snapping

**Passed**: the owner accepted it on 2026-09-28 after retesting the
stickier snapping ("good for now, we can always tweak it later").

Decisions:

- **Window capture is direct** (PRD §7.3): `CreateForWindow`, so covered
  parts of the window are included and nothing in front of it is. If a
  window refuses, Framecut cuts its visible part from the frozen screen
  and logs a warning.
- **Window bounds come from DWM** (`DWMWA_EXTENDED_FRAME_BOUNDS`), not
  `GetWindowRect`, which includes invisible resize borders (711×810 vs the
  visible 693×801 for Calculator). Window capture frames are exactly the
  DWM bounds (measured on Calculator and the About Windows dialog).
- **Rounded corners are transparent.** Window frames carry premultiplied
  alpha at the corners and the 1 px border; the shader un-premultiplies,
  converts the colour, and keeps the coverage. The PNG keeps it; the
  clipboard bitmap is composited over white, because many applications
  ignore bitmap alpha. Monitor captures are forced opaque.
- **Display capture** is Window mode over the desktop (no window under the
  pointer). The Capture Bar (step 3) adds an explicit Display button.
- **Snapping** holds each axis of the drag to a window edge or monitor
  edge: it catches within 10 logical pixels and lets go only beyond 24, and
  each held side of the selection shows a 3 px blue line. Only edges
  visible at the pointer count: an edge hidden behind a window in front
  does not pull. On by default (`snap_to_windows` in settings; PRD open
  question 6). The first version caught within 6 pixels with no hold; the
  owner found it too subtle ("a touch stickier"), 2026-09-28.

Automated: hit-testing front to back; snapping (near, far, hidden edges,
corners, monitor edges); overlay UI tests for Space, hover highlight with
dimensions, clicking a window (including one hanging off the monitor),
clicking the desktop, snapping on and off; GPU test for premultiplied
corners; un-premultiplying 8-bit frames; bitmap compositing.

Smoke test (release, scratch data folder), 2026-09-27: hotkey, Space, click
on Calculator → 693×801 PNG with transparent corners (corner alpha 60,
centre 255) on the clipboard and on disk 63 ms after the click (window
capture 60 ms on the capture thread). A drag starting 4 px right and 3 px
below Calculator's corner snapped to it. The direct capture and the frozen
screen's cut of the same pixels are **identical** in all 30,471 opaque
pixels compared (HDR on), so window capture uses the same colour transform.

Owner's results, 2026-09-28: items 2, 3 (a partly covered terminal came
out whole, without the windows in front), 6 pass; 7: keep transparent
corners (the terminal paste showed white corners because it reads the
bitmap; the saved PNGs have them transparent); 4: there was no desktop to
click with windows covering the screen, so Display waits for the Capture
Bar; 5: works but too subtle, now stickier and visible. Retest passed; a
desktop click captured the whole monitor with its windows, as intended
(a Display screenshot).

Acceptance (manual):

1. Start Framecut. Open a few windows, overlapping.
2. **Ctrl+Alt+S**, then **Space**. The hint at the top changes to Window
   mode and the window under the pointer is outlined in blue with its size.
   Move over other windows: the outline follows. Move over the desktop (or
   a spot with no window): the whole display is outlined.
3. Click a window that is **partly covered** by another. Paste: the whole
   window, including the covered part, with rounded corners. Pasted into a
   chat or browser, the corners are transparent; in Paint they are white.
4. **Ctrl+Alt+S**, Space, click the desktop: the whole monitor is copied.
5. **Ctrl+Alt+S**, and drag starting just inside a window's corner. The
   selection jumps to the window's edge. Drag towards another window's edge:
   it snaps there too.
6. **Ctrl+Alt+S**, Space, Space: back in Area mode; drag works as before.
7. Say whether the transparent corners are what you want, or whether you
   would rather have square corners filled with what was behind the window.

### Step 3: the Capture Bar

**Passed**: the owner ran all eight acceptance steps on 2026-09-28 ("I
really like being able to drive it all via keyboard").

Decisions:

- **Hotkeys.** The Capture Bar has its own hotkey, **Ctrl+Alt+C**
  (`capture_bar_hotkey`); **Ctrl+Alt+S** stays the direct area screenshot
  the owner already uses (PRD §7.1: one master hotkey plus optional direct
  ones). A tray click now opens the Capture Bar, the primary entry point;
  the tray menu has both.
- **Placement.** 312×132 logical pixels, centred 24 pixels below the top of
  the monitor under the pointer, like Snipping Tool's toolbar. Rounded
  corners (DWM), and excluded from capture (`WDA_EXCLUDEFROMCAPTURE`), so it
  can never appear in a screenshot. `FRAMECUT_CAPTURABLE_UI=1` keeps it
  capturable, for screenshots of Framecut itself.
- **Keyboard.** Arrows and Tab move, Enter or Space takes the selection,
  A / W / D choose directly, Escape closes. Losing focus closes it too.
- **Memory.** It opens on the last target used (`last_target`), so Enter
  repeats the last kind of capture (PRD §7.5).
- **Display** captures the monitor at once, with no overlay.
- **Record** is shown, disabled and marked "soon", until Milestone 3.

Automated: UI tests for opening on the last target, arrows/Tab wrapping,
letters, clicking a target, Escape (and nothing after it), losing focus;
the bar's physical rectangle; the tray menu; the embedded icons.

Smoke test (release, scratch data folder), 2026-09-28: Ctrl+Alt+C shows
the bar 20–33 ms after the hotkey, 468×198 physical at (1686, 36) on the
150% monitor, focused, display affinity `WDA_EXCLUDEFROMCAPTURE`. D gave a
3840×2160 PNG 145 ms later, `last_target` became `display`, and Enter
repeated it. Escape closed the bar with nothing captured. A tray click
opened it; W went to the overlay in Window mode.

Acceptance (manual):

1. Press **Ctrl+Alt+C**. The Capture Bar appears at the top of the monitor
   your pointer is on.
2. Click **Display**. The whole monitor is copied (and saved).
3. Press **Ctrl+Alt+C** again: Display is highlighted (remembered). Press
   **←** to Window, **Enter**: Window mode, click a window.
4. Press **Ctrl+Alt+C**, then **A**: straight into an area selection.
5. Press **Ctrl+Alt+C**, then click somewhere else on the screen: the bar
   closes. Again with **Escape**.
6. Click the tray icon: the bar opens. Right-click it: the menu lists
   **Capture Bar** and **Screenshot an area** with their hotkeys.
7. **Ctrl+Alt+S** still goes straight to an area selection.
8. Say what you think of the bar's look and placement.

### Step 4: thumbnail, notifications, capture exclusion

**Passed**: the owner accepted it on 2026-09-28 ("good enough to move on")
after the retests and the drag-image look below.

Owner's results, 2026-09-28: items 1, 3, 4, 5, 6 pass ("feels pretty good
overall"; dragging into Claude Code worked). Two bugs, both fixed in
`ff28e31` and verified live, to retest:

- **2: after hovering, the thumbnail never closed.** GPUI updates an
  element's hover state only on mouse moves inside the window; once the
  pointer left, no move arrived and the card stayed "hovered". The
  countdown now asks Windows where the pointer is (a probe; the headless
  test reproduces the case with no mouse event). Live: kept while hovered,
  closed about 5 s after the pointer left.
- **4: the drag image was sometimes a white square**, the first drag after
  starting. The Shell drew the file's thumbnail from its cache, not ready
  for a file written a moment earlier. Framecut now sets the drag image
  from the screenshot (`IDragSourceHelper::InitializeFromBitmap`). Live:
  the first drag after a fresh start shows the capture, upright, centred
  on the pointer.

Retest, 2026-09-28: both fixed. Two more, fixed in the next commit:

- **The × lagged** behind the pointer by up to a second: the pointer was
  checked once a second. Now every 100 ms.
- **Two drag styles** in the same target (Telegram): sharp, or faded with
  blurred edges. Reproduced by capture shape: Windows fades drag images
  larger than about 256 pixels (a wide capture gave a 360-pixel image).
  The drag image is now capped at 256 pixels; the same wide capture drags
  sharp.

The owner then preferred a softer look than sharp but a gentler one than
Windows' fade, which measured as opacity falling in a straight line from
the centre on each axis, at about 74% overall; the Shell's drag helper
applies that 74% to every drag image and cannot be told otherwise. So
Framecut now draws the drag image itself, in a click-through layered
window that follows the pointer (`DoDragDrop` with its own `IDropSource`).
Chosen from side-by-side drags, 2026-09-28: sharp, 90% opaque, a 20-pixel
soft edge (half inside, half outside, so it reads as a blurred edge, not
an inner shadow), squircle corners. Three comparison rounds: an inward
edge fade read as an inset shadow; a copy of Windows' radial fade was
too faint and banded.

Decisions:

- **The thumbnail never takes the keyboard** (`WS_EX_NOACTIVATE`, shown
  without activation). Ctrl+V straight after a capture must paste into the
  application in front; a focused thumbnail would swallow it. So the
  thumbnail is mouse-only: click opens, drag shares, × closes, and it
  closes itself. This is the one surface not driven by keyboard.
- **Placement and size.** Bottom-right of the monitor's work area (above
  the taskbar), 16 logical pixels in. The image fits 240×150 logical
  pixels, never enlarged, at least 64 on a side, with 6 pixels of padding.
- **Countdown.** `thumbnail_seconds` (6, PRD open question 5), paused
  while the pointer is over it. A new screenshot replaces the old
  thumbnail; it never blocks the next capture.
- **Click** opens the file in the default image viewer. **Drag** hands the
  Shell's own data object for the file to `SHDoDragDrop`, so Explorer,
  browsers and chat applications receive a normal file drag, with the
  standard drag image. The card closes after a drop.
- **Without auto-save**, the file is written to `%TEMP%\Framecut` only when
  the thumbnail is opened or dragged; those files are deleted after a day.
- **Notification** after each capture is available but off
  (`notify_after_capture`): the thumbnail already confirms the capture.
- **Capture exclusion.** The Capture Bar and the thumbnail are excluded
  from every capture (`WDA_EXCLUDEFROMCAPTURE`). The selection overlay is
  not: it shows a frozen image, so it can never contaminate Framecut's own
  output, and a screen recorder may want to show it.

Automated: UI tests for click → open, press-and-move → one drag (jitter
still a click), hover shows ×, × closes, the countdown closes once, the
countdown pauses while hovered; image sizing; card placement; temporary
file cleanup.

Smoke test (release, scratch data folder), 2026-09-28: after an area
capture the card is 318×243 physical at (3498, 1821), display affinity
`WDA_EXCLUDEFROMCAPTURE`, `WS_EX_NOACTIVATE`, and the foreground window is
unchanged. It closed itself after the countdown. Dragged into an Explorer
window on an empty folder, the PNG arrived there and the card closed. The
card, imaged with `FRAMECUT_CAPTURABLE_UI`, shows the capture with rounded
corners and the × on hover.

Acceptance (manual):

1. Take an area screenshot (**Ctrl+Alt+S**). A thumbnail appears
   bottom-right. Press **Ctrl+V** in the app you were in: it pastes (the
   thumbnail did not take the keyboard).
2. Leave it: it disappears after about 6 seconds. Take another and hold the
   pointer over it: it stays until you move away.
3. Take another and **click** it: the image opens in your image viewer.
4. Take another and **drag** it into a chat (Discord, Slack, a browser
   upload box) or an Explorer folder: the file arrives.
5. Take another and click the **×** that appears on hover.
6. Take two screenshots quickly: the second thumbnail replaces the first.
7. Say whether the thumbnail's size, position and 6 seconds feel right.

### Step 5: the settings window

Decisions:

- **Built on GPUI Kit's settings component**: a searchable sidebar, pages
  (General, Screenshot, Diagnostics, as PRD §26 groups them) and groups.
  It follows the Windows light/dark setting. Every change applies and is
  saved at once; there is no Save button.
- **General**: start at sign-in (the per-user `Run` key, which Task
  Manager's Startup apps page shows too; not stored in `settings.json`),
  the Capture Bar and area-screenshot hotkeys, copy to the clipboard, save
  to the folder, and the folder (Change… picks one, Open shows it).
- **Screenshot**: include the pointer (PRD §15, off by default: the
  pointer as it was when the hotkey was pressed), snap to window edges,
  show a thumbnail, how long it stays (3, 6, 10 or 20 seconds), show a
  notification.
- **Diagnostics**: version, Windows build, each display (size, scale,
  HDR / Advanced Color / SDR, SDR white level, GPU), and buttons to open
  the log folder and the settings file. No tone-mapping controls (§26).
- **Hotkeys are recorded**, not typed: select the field and press Enter
  (or click it), then press the new keys; Escape cancels. The global
  hotkeys are paused meanwhile, so pressing the current one does not start
  a capture. A hotkey without Ctrl, Alt or Win, the other hotkey, or one
  another application owns is refused with a message, and the old one is
  kept. Recording hotkeys arrive with recording.
- **Opening it**: the tray menu's **Settings…**, and starting Framecut
  again (from the Start menu, say), which now shows the settings instead
  of the step 1 "already running" notification.
- **Keyboard**: Tab and Shift+Tab move between controls, Space and Enter
  use them, Escape closes the window.

Automated: UI tests (fake hooks) for the hotkeys shown, recording a new
one (pause, apply, save), a hotkey another application owns (refused,
old one restored), the other hotkey and a Shift-only one (refused),
Escape cancelling a recording and then closing the window; key presses to
hotkeys; the startup entry round trip (under a test name, never
Framecut's own).

Smoke test (release, scratch data folder), 2026-09-28: starting Framecut
again opened the window, in front and focused; Escape and the title bar's
close button both closed it with no errors in the log and Framecut still
running. Recording Ctrl+Alt+X for the Capture Bar saved it; afterwards
Ctrl+Alt+C did nothing and Ctrl+Alt+X opened the Capture Bar.

Two problems found on the way, both fixed: the window stayed hidden when
Framecut was started with a "start hidden" request (the smoke test's);
it is now shown explicitly. And GPUI's own close path logged errors as
the window went; Escape and the close button now hide it first and remove
it a moment later, as the overlay does.

Acceptance (manual):

1. Right-click the tray icon → **Settings…**. The window opens in front.
2. Press **Tab** a few times: focus moves through the controls. Press
   **Escape**: the window closes.
3. Start Framecut again from a second terminal: the settings window
   opens.
4. **Hotkeys**: select "Open the Capture Bar", press Enter, press a new
   shortcut (say Ctrl+Alt+X). Close the window; the new shortcut opens the
   Capture Bar and the old one does nothing. Try Shift+X, and the other
   hotkey's keys: both are refused with a message. Set it back.
5. **Screenshot → Include the pointer**: on. Take a screenshot: the
   pointer is in it. Turn it off again.
6. **Thumbnail stays for**: 3 seconds. Take a screenshot: the thumbnail
   leaves sooner. **Show a notification**: on, take one, see it; off.
7. **Folder → Change…**: pick another folder; the next screenshot is saved
   there. Change it back.
8. **Start Framecut when you sign in**: on. Task Manager → Startup apps
   lists Framecut. (Turn it off unless you want it.)
9. **Diagnostics**: your two displays, HDR on, their SDR white levels and
   the GPU. **Open** next to the log folder opens it.
10. Say what you think of the window's layout and wording.

## Milestone 1: area screenshot

Exit criterion (PRD §31): *hotkey → drag → release → paste into
browser/chat, with visually correct SDR output.*

### Automated

| Suite | Covers |
|---|---|
| `framecut` unit tests | Logical → physical conversion: 100–200% scale, any drag direction, snapping to physical pixels, clamping to the monitor, empty clicks |
| `framecut` `tests/ui.rs` | The real overlay in headless GPUI windows with native pointer and keyboard events: live physical dimensions at 100/125/175/200%, drags in any direction, Escape and right-click cancel, a click selects nothing, the overlay takes focus |
| `framecut-capture` unit tests | Crop bounds, empty and out-of-bounds regions, PNG round trip |
| `framecut-platform` unit and live tests | Hotkey parsing; registering a hotkey and reporting a conflict; the `CF_DIBV5` layout |

Two live tests touch your machine and are opt-in:

```powershell
# Captures the screen in memory and times the freeze and the cut.
mise exec -- cargo test --release -p framecut-capture --test service -- --ignored --nocapture
# Replaces the clipboard with a test image.
mise exec -- cargo test -p framecut-platform --test platform -- --ignored
```

Measured on 2026-09-27 (release build, 3840×2160 HDR, RTX 4090): the overlay is
on screen **130 ms** after the hotkey (freeze 70 ms on the capture thread); a
640×360 cut and its PNG take under 1 ms.

The first smoke test checked the overlay *window* rectangle and that the app
had exited *after* the quit hotkey, and so missed two bugs the owner found
at once: the app quit whenever the overlay closed (GPUI's default quit mode),
sometimes before the clipboard was written, and the overlay's drawable area
was 11 px short of three screen edges (Windows frame borders). Both are fixed
(`e49126f`, `d020336`). The smoke test now checks the **client** area is
(0,0) 3840×2160, that the process is alive 0.1, 0.5 and 2 s after the overlay
closes (on Escape and on release), that the clipboard changed after a drag,
and that only the quit hotkey ends the app.

### Acceptance (manual)

1. Start it: `mise exec -- cargo run --release -p framecut`. No window
   appears; the terminal says which hotkeys are active.
2. Put something recognisable on screen: a light-theme page in a browser, VS
   Code, or `fixtures/sdr-reference.html`.
3. Press **Ctrl+Alt+S**. The screen freezes and dims; the pointer is a
   crosshair.
4. Drag a rectangle. The area inside is undimmed, outlined, and its size is
   shown in physical pixels. Release.
5. Paste into a browser page that accepts images (e.g. a GitHub comment box),
   Slack, Discord, or Paint. It should look exactly like the area you
   selected: whites white, text crisp, no washed-out or grey cast.
6. Press Ctrl+Alt+S again and press **Escape**: the overlay closes and the
   clipboard is unchanged. Repeat with a right-click.
7. With HDR on, repeat step 3–5 on a light-theme page. With HDR off, repeat
   once more. Both should look the same.
8. Quit with **Ctrl+Alt+Shift+Q**.

## Milestone 0: the HDR-off / HDR-on gate

About ten minutes. The `gate` command guides the session: it tells you what to
set, checks with Windows that the setting took effect before capturing, and
writes one `summary.md` at the end.

### What it captures

Two scenes, each in four display states, eight captures in all:

| State | Windows setting | Role |
|---|---|---|
| `hdr-off` | HDR off | the reference: Windows' own 8-bit desktop |
| `hdr-low` | HDR on, SDR content brightness **10** | |
| `hdr-mid` | HDR on, SDR content brightness **50** | |
| `hdr-high` | HDR on, SDR content brightness **100** | |

| Scene | What to show |
|---|---|
| `fixture` | `fixtures/sdr-reference.html` full screen: neutrals, every grey/R/G/B code, saturated and UI colors, gradients, light and dark code, text |
| `mixed` | the same page with `?hdr`: adds a BT.2020 PQ test image in the bottom-right corner |

The low setting is 10, not 0, on purpose. If the slider maps 0 to 80 nits, `S`
is 1 and a bug that ignores `S` would pass there. The gate reads the real white
level at each step and refuses to continue if two states share one.

### Before you start

1. Build: `mise exec -- cargo build --release -p capture-spike`
2. Night light off, and Do Not Disturb on (Win+N, then the bell): a
   notification in a capture fails the comparison. Keep the same resolution
   and scaling (currently 3840×2160 at 150%) throughout.
3. Open the two scenes as two tabs of one Edge window. Edge is the browser the
   HDR test image was verified in. From PowerShell in the repository root:

   ```powershell
   $page = (Resolve-Path fixtures\sdr-reference.html).Path -replace '\\','/'
   Start-Process msedge -ArgumentList "file:///$page", "file:///${page}?hdr"
   ```

   Set zoom to 100% (Ctrl+0), then press **F11** for full screen. Ctrl+Tab
   switches tabs in full screen. Nothing on the page moves, so it looks the
   same every time you come back to it.
4. Park the pointer at the screen's edge. It is not captured, but hover states
   are.

### Run it

In a terminal in the repository root:

```powershell
.\target\release\capture-spike.exe gate
```

At each step it prints what to set, and waits for Enter:

- **HDR on/off:** Settings > System > Display > HDR > *Use HDR*, or press
  Win+Alt+B.
- **SDR content brightness:** Settings > System > Display > HDR > *SDR content
  brightness*.

For each scene it prints `Show ...`: press Enter, switch to Edge (Alt+Tab),
select the scene's tab, and leave it on screen. The capture happens 5 seconds
later, and the terminal beeps when it is done. Take longer with `--delay 8`.
If a capture fails it says why and asks again. After each capture it also
checks the right tab was showing: `fixture` must hold no HDR content, `mixed`
must (except at brightness 100, where a panel may have no headroom left), and
no two scenes of a state may be identical. If one looks wrong it offers to
retake it. Type `q` at any prompt to stop.

When the four states are done it writes
`captures\gate-<timestamp>\summary.md` and prints it. To re-run only the
analysis: `capture-spike gate-report captures\gate-<timestamp>`.

### Reading the result

The verdict at the end is **PASS** only when all of these hold:

- the session is valid: reference not in HDR, three distinct SDR white levels,
  each scene showing what its name says (otherwise the verdict is
  **UNDECIDED**);
- every HDR state matches the reference, per scene: mean ΔE00 ≤ 0.5, 99th
  percentile ≤ 1.0, and no 64×64 block averaging over 1.0;
- the three HDR states match each other by the same measure.

In a frame with HDR content, the region of that content is left out: tiles
where a quarter of the pixels are not SDR content, grown by one tile and filled
to each area's bounding box. With HDR off, Edge tone maps the image itself,
which a capture cannot reproduce. The captures table gives the region's
bounds, so you can check it is the image and nothing else. Without HDR content
nothing is left out.

The report re-converts HDR captures from their saved FP16 frames with the
current transform, so an old session can be re-analysed after a fix.

The mean and percentile thresholds were fixed before any measurement. The
block limit was added after the first session, where a wrong 768×384 region
passed the other two because it covered under 1% of the frame. The expected
result is an exact match: the model predicts identical codes, and SDR content measured
100% on the code grid. A systematic ±1-code tint fails on purpose: it is a
model error even though it is below one just-noticeable difference.

The summary also holds:

- Windows' own 8-bit capture compared with the reference. It is the naive
  result and is expected to fail badly; it shows what the gate would catch.
- **How Windows placed SDR codes**, measured against the reference: the median
  FP16 value for each grey code, next to what piecewise sRGB and gamma 2.2
  predict. If this disagrees with the model, the transform is wrong; if it
  agrees and a comparison still fails, the application drew different codes in
  the two modes.
- **Highlights** in the mixed scene: how many pixels exceed SDR white, and how
  many distinct output colors they keep with the shoulder versus clipping.

`diffs\` holds a heatmap per comparison: the reference dimmed, red where it
differs (full red at ΔE00 4), blue where left out. A passing heatmap is plain
dim grey, with blue only on the HDR image.

### What to send back

1. The whole `summary.md`.
2. A look, not a measurement: open `hdr-mid\mixed\capture.png` and
   `hdr-off\mixed\capture.png` at 100% and say whether the page looks the
   same, and whether the HDR image's bright steps and textured strips show
   detail or flat white.
3. Only if something fails: the `diffs\*.png` heatmaps of the failing lines.
   They show the test page only.

Keep the rest (`source.fp16` is 66 MB per capture). Those files allow a
`transfer`, `probe` or `convert` re-analysis without capturing again.

### Retaking captures

To retake some states of a session, point `--out` at it and name the states.
The rest of the session is kept, and the report covers all of it:

```powershell
.	argeteleasepture-spike.exe gate --out capturesgate-20260927-112049 --states hdr-low,hdr-mid
```

Add `--scenes` to retake only some scenes, e.g. `--states hdr-mid --scenes
fixture`. The first session needed both: two captures showed the wrong tab,
and the retake of `hdr-mid/fixture` caught a notification (a 480×120 block in
the bottom-right corner; every other pixel matched the reference exactly).

## HDR content benchmark

The gate proves ordinary UI. How HDR content itself (HDR images, video,
games) should look in an SDR screenshot has no single right answer: a PNG
cannot be brighter than white, so highlights must be squeezed below it. This
section records how the current choice was made, so it can be revisited.

### What other tools do (research, 2026-09-27)

| Tool | HDR → SDR | White UI |
|---|---|---|
| NVIDIA (Alt+F1) | Saves JXR; its PNG was reported not tone mapped (washed out) | — |
| ShareX | None: washed out | — |
| OBS | Reinhard per channel | greyed to ≈ 191 |
| Snipping Tool, colour corrector off | Clips highlights; UI washed out on HDR (measured below) | 255 |
| Snipping Tool, colour corrector on | Tone maps | greyed to 224 (measured below) |
| Xbox Game Bar | JXR plus a tone-mapped PNG; method unpublished | — |
| Chrome / Edge (HDR off) | Documented curve: HDR reference white (203 nits) → half brightness, ≈ code 188 | 255 |
| ITU-R BT.2446 (broadcast) | HDR reference white → 86–96% of SDR | — |

No established test method for HDR screenshots exists; objective metrics
(TMQI, HDR-VDP, ΔE_ITP) are for media, not desktop UI.

### Measured on the `?hdr` page, SDR brightness 50

| Candidate | Page white | UI vs HDR-off | HDR image vs Edge's HDR-off rendering | Eight grey steps, 100–500 nits |
|---|---|---|---|---|
| Edge, HDR off (reference) | 255 | — | — | 136 164 187 201 211 218 223 232 |
| Snipping Tool, corrector off | 255 | ΔE00 3.06 | 9.26 | all 255 |
| Snipping Tool, corrector on | 224 | ΔE00 5.79 | 4.98 | 173 203 215 220 223 225 226 228 |
| Framecut v3 | 255 | ΔE00 0.00 | 4.48 | 159 191 218 236 248 251 252 253 |
| Framecut v4 (chosen) | 255 | ΔE00 0.00 | 0.90 | 143 171 195 207 216 222 227 235 |

The owner compared these as images and chose v4. Still to do, in the owner's
A/B style: real HDR video and photos (e.g. a paused YouTube HDR clip in Edge,
HDR on and off), multiple HDR windows, subtitles and controls over video, and
a steadier exposure anchor than the region's peak (see
[COLOR_PIPELINE.md](COLOR_PIPELINE.md#known-limits-and-expected-differences)).

## Manual tools

| Command | Use |
|---|---|
| `capture-spike list` | Monitors, mode, SDR white level, `S` |
| `capture-spike capture [--monitor DISPLAY1] [--delay 5]` | One capture into `captures\` |
| `capture-spike compare A.png B.png [--source B.fp16] [--roi X,Y,W,H] [--heatmap D.png]` | ΔE00 comparison; `--source` leaves out the HDR content region |
| `capture-spike transfer REF.png SRC.fp16` | Measured SDR transfer against an HDR-off reference |
| `capture-spike fit SRC.fp16 [--roi …] [--map M.png]` | The same without a reference: how close values sit to 8-bit codes |
| `capture-spike probe SRC.fp16 X,Y,W,H` | Exact values over a region |
| `capture-spike convert SRC.fp16 OUT.png [--highlights clip]` | Re-run the transform on a saved frame |

Coordinates are physical pixels from the captured monitor's top-left corner.

## Matrix

| Case (PRD) | Status | Evidence |
|---|---|---|
| Monitor enumeration, Advanced Color / HDR state, SDR white level (§8.2–8.4) | Passed locally | `list`: 4K HDR, level 3000 (240 nits), mode from `ADVANCED_COLOR_INFO_2` |
| WGC FP16 capture → shader → sRGB PNG (§8.5, §9.2) | Passed locally | 4K in 68 ms + 9.5 ms |
| Shader arithmetic (§33 golden tests) | Passed | WARP and RTX 4090 |
| Windows stores SDR as `S × sRGB⁻¹(code)` | Passed | 100.00% on the code grid (terminal); against HDR-off references at `S` = 1.5, 3.5, 6: within 0.17 code, white / `S` = 1.00000 |
| SDR content untouched beside HDR content (§9.6) | Passed locally | Mixed scene: only the HDR image changes |
| Highlight texture kept (§9.6, §34.2) | Passed on the test image; owner chose the v4 curve | 106 distinct colors vs 8 clipped at 3.79×; ΔE00 0.51–1.07 vs Edge's own rendering. Real HDR video still to test ([benchmark](#hdr-content-benchmark)) |
| **HDR off vs on, fixture (§32.1, §34.2)** | **Passed** | Identical, every pixel, at `S` = 1.5, 3.5 and 6 |
| **SDR brightness 10 / 50 / 100 invariance (§32.2)** | **Passed** | All pairs pass; slider = 80 + 4 × value nits |
| Light-theme browser (§31) | Passed | Fixture |
| Light-theme IDE, dark-theme IDE (§31) | As the fixture (its code blocks); optionally add `--scenes fixture,mixed,vscode-light,vscode-dark` | |
| Saturated colors (§31) | Passed (with HDR on screen, since the v3 cross-talk fix) | Fixture |
| HDR content beside SDR UI (§31) | Passed | Mixed scene: ≤ 1 code outside the HDR image at all three levels |
| SDR monitor path (§9.5) | Passed | HDR off: FP16 path and 8-bit capture identical |
| Advanced Color SDR (WCG) hardware | Not available | `S = 1` from the SDK's description |
| 125/150/200% scaling, second monitor (§32.3) | Milestone 5 | |
| **Milestone 1: hotkey → drag → release → paste (§31)** | **Passed: owner's acceptance, 2026-09-27** | Three screenshots in one session; clipboard 18–34 ms after release; overlay 90–171 ms after the hotkey; both monitors; Escape and right-click cancel; quit; HDR on vs off pastes match (VS Code, mean ΔE00 0.022 on downscaled copies) |
| Background app, no window until invoked | Passed locally | Smoke test: no window before the hotkey |
| Global screenshot hotkey; conflict reported | Passed locally | Live test; smoke test |
| Frozen overlay on the monitor under the pointer, exact bounds, focused | Passed locally | Client area (0,0) 3840×2160, foreground; regression test in `framecut-platform` |
| App keeps running after a capture or cancel | Passed locally | Alive 0.1–2 s after the overlay closes; quit hotkey ends it |
| Live physical-pixel dimensions (§7.2, §20) | Passed (headless) | UI tests at 100/125/175/200% |
| Escape and right-click cancel | Passed (headless + smoke) | UI tests; smoke test (Escape) |
| Crop and PNG from the frozen, converted frame | Passed locally | Live service test |
| Clipboard as PNG + bitmap, retry on contention (§11) | Passed (unit); live test opt-in | Owner's paste test is the real check |
| Overlay within ~150 ms (§22.2) | Passed locally | 130 ms, release build, 4K HDR |
| Tray icon and menu; left click takes a screenshot (§7.1) | **Passed: owner, 2026-09-27** | Smoke test (tray click); owner ran all 7 acceptance steps |
| Auto-save to `Pictures/Framecut`, unique names (§12) | Passed locally | Unit tests; smoke test: two files |
| Settings on disk, survive a broken file | Passed (unit) | |
| Log file (§28) | Passed locally | Version, Windows build, monitors, timings; no errors |
| Single instance | Passed locally | Second launch exits 0; the first notifies |
| Space toggles Area ↔ Window; window hover highlight (§7.3, §32.6) | **Passed: owner, 2026-09-28** | UI tests; smoke test |
| Window capture via `CreateForWindow`, same colour as the screen (§7.3) | Passed locally | 693×801 Calculator; identical to the frozen cut |
| Display screenshot (§8.1) | **Passed: owner, 2026-09-28** | Desktop click in Window mode |
| Boundary snapping (§7.2) | **Passed: owner, 2026-09-28** | Catch 10, release 24 logical px; blue side markers |
| Capture Bar: master hotkey, Area / Window / Display, keyboard, remembers the last target, closes on start (§7.5, §32.6) | **Passed: owner, 2026-09-28** | UI tests; smoke test; all eight acceptance steps |
| App UI excluded from capture (§7.6) | Capture Bar and thumbnail: passed locally | Display affinity 0x11 |
| Post-capture thumbnail: after the clipboard, auto-dismiss, click opens, drag into apps, excluded, never blocks the next capture (§7.6, §32.6) | **Passed: owner, 2026-09-28** | UI tests; drags into Explorer, Telegram, Claude Code |
| Optional notification after capture (§8, settings) | Built, off by default | `notify_after_capture` |
| Settings window: General / Screenshot / Diagnostics, keyboard, applies at once (§26) | Passed (headless + smoke); awaits the owner | UI tests; smoke test |
| Include cursor (§15) | Built, off by default; awaits the owner | `include_cursor` |
| Launch at sign-in (§26) | Passed (unit, under a test name); awaits the owner | Run key |
| Diagnostics: monitors, HDR, SDR white, GPU (§26) | Built; awaits the owner | |
