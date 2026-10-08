# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.22](https://github.com/jakeklassen/shuttercrab/compare/v0.1.21...v0.1.22) - 2026-10-08

### Added

- Text actions: copy and redact a screenshot's text
- *(platform)* read small text twice as large, smoothly
- *(platform)* read the text in an image with Windows' OCR

### Fixed

- copied text says [REDACTED], in reading order
- Quick redact finds an email OCR misread or split

## [0.1.21](https://github.com/jakeklassen/shuttercrab/compare/v0.1.20...v0.1.21) - 2026-10-07

### Added

- Recordings open in Shuttercrab's window when they are saved, ready to play, as Snipping Tool does. Each opens paused on its first picture, with a play bar at the bottom: play or pause, the time, a slider to move through it, its length, and the volume, whose button opens a slider with mute. Keys: Space plays or pauses, Left and Right go five seconds back or forward, Home and End go to the start and the end, Up and Down change the volume, and M mutes. While playing, the bar fades and comes back when the pointer moves. The volume is remembered. Settings > Recording > "Open in Shuttercrab's window" turns this off.
- With a recording open, Copy (Ctrl+C) puts the video file on the clipboard, to paste into a folder or a chat, and Save as (Ctrl+S) saves a copy where you choose. The ⋯ menu has Show in folder and Open with. Clear (X, or Ctrl+W) closes the recording, and Ctrl+Z brings it back where it was.

## [0.1.20](https://github.com/jakeklassen/shuttercrab/compare/v0.1.19...v0.1.20) - 2026-10-07

### Added

- In a window recording, the system sound can be only that window's app: a game or a video, without your music, calls or notifications. Beside the speaker switch on the recording bar, "This app" or "All sound" (B) chooses, before and while recording; Settings > Sound sets which it starts with (this app, by default).

### Fixed

- Recordings with all system sound had the sound about a fifth of a second behind the picture. It is in sync now, and switching between this app's sound and all of it leaves no gap.

## [0.1.19](https://github.com/jakeklassen/shuttercrab/compare/v0.1.18...v0.1.19) - 2026-10-07

### Added

- Clear (the X after Undo and Redo, or Ctrl+W) takes the screenshot away and puts the window back to its start view. Nothing is asked first: Ctrl+Z, or "Bring the screenshot back", restores it with your markup, and the window exactly where it was, until your next capture.
- The window's toolbar fits narrow windows, as Snipping Tool's does: narrower than one row of tools, the drawing tools, crop, Undo, Redo and Clear move to a bar at the bottom, and the capture targets fold into one menu. The window no longer shrinks so far that tools are cut off.

## [0.1.18](https://github.com/jakeklassen/shuttercrab/compare/v0.1.17...v0.1.18) - 2026-10-07

### Added

- Record a window (W in Record mode): click a window, and the recording follows it wherever it goes. Only that window is recorded, so whatever passes in front of it stays out.
  - Resize it, or maximise it, and the video keeps the size it started at, with the window fitted inside and dark bars where its shape no longer matches.
  - Minimise it and the recording pauses, with the reason on the bar; restore it and recording carries on. Apps that hide their window instead pause the same way.
  - Close it and the recording stops and is saved, and the notification says why. Closed before recording starts, the ready bar or the countdown simply goes away.
  - The dashed border and the recording bar move with the window, and the pointer is left out while you drag or resize it, so it does not jump about in the video.

### Fixed

- Windows' busy "loading" pointer no longer shows in the first seconds of a recording.

## [0.1.17](https://github.com/jakeklassen/shuttercrab/compare/v0.1.16...v0.1.17) - 2026-10-06

### Added

- Recordings can have sound: what your speakers or headphones play, your microphone, or both, mixed into one track. Both are off unless switched on in Settings > Sound, so nothing is recorded by surprise; Settings also chooses which microphone.
- A ready bar before each recording, as in Snipping Tool: after you choose the area, the bar waits with Start (Enter) and Cancel (Esc), and switches for the microphone (M) and the system sound (A) that start from your settings and apply to that recording only. The microphone list (Down) chooses which one, and remembers it for next time. The switches stay while recording, so either sound can be turned on or off mid-take. A microphone that is off is never opened.

### Fixed

- Clicking a button on the recording bar works the first time. Before, the click that gave the bar the keyboard moved its buttons, so it was missed, and a second click could land on Pause instead.

## [0.1.16](https://github.com/jakeklassen/shuttercrab/compare/v0.1.15...v0.1.16) - 2026-10-06

### Fixed

- In the window's menus (More and the delay), Tab moves to the next item and Shift+Tab to the one before. Tab did nothing there before.

## [0.1.15](https://github.com/jakeklassen/shuttercrab/compare/v0.1.14...v0.1.15) - 2026-10-06

### Added

- Crop, as in Snipping Tool: the crop button (or C) shows the whole screenshot with a frame on the part kept. Drag a corner or an edge to resize it, the inside to move it; Tab and the arrow keys do the same from the keyboard, and the frame's size shows as it changes. Apply (Enter) keeps it, Cancel (Esc) leaves it as it was. Nothing is lost: opening crop again can widen it, and undo takes a crop back. Copy and Save give the cropped image.
- Over the crop frame, the pointer shows which way each corner and edge drags. Over a zoomed-in screenshot it shows an open hand where it can move (hold Space, or put the tool down), and a closed hand while moving it.
- A setting to turn the window's single-key shortcuts (N for New, P for the pen and the like) on or off, in Settings > General. They're off for now, so a stray key press does nothing; Ctrl shortcuts and Escape always work.

### Fixed

- A drag that ended over a button, such as Apply, Discard or a toolbar button, no longer pressed it. Buttons now need a click.
- Pressing a key no longer hides the pointer.

## [0.1.14](https://github.com/jakeklassen/shuttercrab/compare/v0.1.13...v0.1.14) - 2026-10-06

### Added

- Shapes, as in Snipping Tool: G, or the Shapes button, shows a bar over the screenshot with Rectangle (R), Oval (O), Line (L) and Arrow (A), then Fill (F) and Outline (T). Each has 30 colours, Transparent first, and an opacity; Outline has a size from 1 to 24. Drag to draw; hold Shift for a square, a circle, or a line at 45°. Your choices are remembered.
- A shape stays selected after you draw it: drag it to move it, a corner to resize it, or the button above it to rotate it. Arrows move it a pixel, Shift+arrows resize it, Alt+Left and Alt+Right turn it 15°, Delete removes it, and right-click offers Delete and quarter and half turns. Fill, Outline and size change the selected shape.
- Select (V) picks up any shape you drew earlier, to change it again.
- Emoji (E) in the Shapes bar: Snipping Tool's 18, which land in the middle of the view, ready to move, resize and rotate.
- Settings has a Notices page, with the licence of the emoji's art (Microsoft's Fluent Emoji).

## [0.1.13](https://github.com/jakeklassen/shuttercrab/compare/v0.1.12...v0.1.13) - 2026-10-06

### Added

- Print Screen can be a hotkey, on its own or with Ctrl, Alt or Shift: click a hotkey in Settings and press it. While Windows keeps Print Screen for Snipping Tool, Settings says so, with a button to the Windows setting that lets it go.
- A second screenshot hotkey, "Screenshot an area with this window" (Ctrl+Alt+Shift+S to start with), takes an area screenshot with Shuttercrab's window left in the picture.

### Fixed

- Starting a capture any way, by hotkey, the tray or the Capture Bar, now hides Shuttercrab's window while you choose, as Snipping Tool does, so it is not in the way. It comes back showing the screenshot.
- When Shuttercrab's window shows a new screenshot, it now takes the keyboard, so Ctrl+C copies it from there.

## [0.1.12](https://github.com/jakeklassen/shuttercrab/compare/v0.1.11...v0.1.12) - 2026-10-05

### Added

- The eraser, as in Snipping Tool: X, or its button in the toolbar, then drag over marks to take them off whole; the screenshot itself is never touched. Press X again, or click the eraser in hand, for Erase all mark-ups (or Enter). Each drag, and Erase all, undoes in one go.
- The pen, highlighter and eraser buttons show their keys, P, H and X.
## [0.1.11](https://github.com/jakeklassen/shuttercrab/compare/v0.1.10...v0.1.11) - 2026-10-05

### Added

- Draw on a screenshot in Shuttercrab's window, with Snipping Tool's pen and highlighter. P picks up the pen, H the highlighter; drag to draw, and hold Shift for a straight line. The highlighter has a slanted chisel tip and blends like highlighter ink, so text shows through it.
- Click the tool in hand, or press its key again, for its colours and size: the pen's 30 colours and the highlighter's 6, as in Snipping Tool. [ and ] change the size as you go, and the outline at the pointer shows it. Your choices are remembered.
- Undo and redo with Ctrl+Z and Ctrl+Y, or the arrows in the toolbar. With a tool in hand, Space+drag or Ctrl+drag moves the screenshot.
- Copy, Save as, Edit in Paint and Open with give the marked-up screenshot. The one saved automatically stays as it was taken.
- Copy now confirms with a check mark once the screenshot is on the clipboard.
## [0.1.10](https://github.com/jakeklassen/shuttercrab/compare/v0.1.9...v0.1.10) - 2026-10-04

### Added

- Screenshots open in Shuttercrab's window, as in Snipping Tool. One taken from the window comes back showing it; one taken with a hotkey or the Capture Bar shows there too, without taking the keyboard from the app you're in, so you can still paste it straight away. Turn this off in Settings, under After a capture, to keep only the thumbnail. Clicking the thumbnail, or a screenshot's notification, also opens it in the window instead of Photos.
- The window shows a screenshot at full size, or at most half the screen for a big one, scaled to fit. Zoom with Ctrl+scroll, Ctrl+plus and Ctrl+minus; Ctrl+0 fits it and Ctrl+1 shows it at full size. Scroll or drag to move around a zoomed-in screenshot.
- Copy (Ctrl+C) and Save as (Ctrl+S) for the screenshot shown, and in the ⋯ menu Edit in Paint (E), Open with… and Show in folder.
## [0.1.9](https://github.com/jakeklassen/shuttercrab/compare/v0.1.8...v0.1.9) - 2026-10-04

### Fixed

- Restart to update could fail over and over, coming back on the old version with the update still on offer. It happened when something opened from Shuttercrab, such as Photos showing a screenshot, was still running, even in the background after its window closed. Shuttercrab no longer starts programs inside its own install folder, so they can't get in the way. Updating to this version still needs anything you opened from Shuttercrab closed first, Photos included.
## [0.1.8](https://github.com/jakeklassen/shuttercrab/compare/v0.1.7...v0.1.8) - 2026-10-04

### Changed

- Nothing you can see: this release reorganises the code so it is easier to read and change. Long functions are now short, named steps; screenshots and recordings share their HDR conversion code; and the code that reads graphics memory directly explains why that is safe. Screenshots convert to exactly the same pixels as before.

### Fixed

- Stopping a recording could wait forever if an earlier command to the recorder had failed partway through.
- When Shuttercrab can't check for updates (for example, a copy that wasn't installed), the log now says why.
## [0.1.7](https://github.com/jakeklassen/shuttercrab/compare/v0.1.6...v0.1.7) - 2026-10-04

### Performance

- Recording costs games less. In Cyberpunk 2077's benchmark at 4K with HDR and the GPU fully loaded, recording now takes about 7.5% off the frame rate instead of about 10%, a little less than OBS on the same capture method. The recorder checks HDR content 15 times a second instead of on every frame, without waiting for the GPU.

### Fixed

- A repeated frame (on a still screen, or at the end of a recording) could let its texture be reused while the encoder still had it.
## [0.1.6](https://github.com/jakeklassen/shuttercrab/compare/v0.1.5...v0.1.6) - 2026-10-03

### Added

- The main window shows which version is running, quietly, at the bottom. When a newer release has downloaded, that line becomes a Restart to update button (or press U), as well as the item in the tray menu.
## [0.1.5](https://github.com/jakeklassen/shuttercrab/compare/v0.1.4...v0.1.5) - 2026-10-03

### Performance

- Much less memory while you choose what to capture. With two 4K monitors, the selection screen used to peak at about 770 MB for the second or two it was open; it now peaks at about 270–300 MB, and Shuttercrab goes back to about 90 MB afterwards as before. Each frozen screen is now held once instead of twice, the Freeform line no longer needs screen-sized drawing buffers, and the UI toolkit no longer allocates them for windows that never use them.
## [0.1.4](https://github.com/jakeklassen/shuttercrab/compare/v0.1.3...v0.1.4) - 2026-10-03

### Changed

- Clicking the tray icon opens Shuttercrab's window, and the tray menu starts with Open Shuttercrab, so a closed window is always one click away. The Capture Bar stays on Ctrl+Alt+C and in the tray menu.
## [0.1.3](https://github.com/jakeklassen/shuttercrab/compare/v0.1.2...v0.1.3) - 2026-10-03

### Added

- A main window, after Snipping Tool's. Start a capture with New (N), pick a screenshot or a recording (S, R) and what to capture: an area, a window, a display or a freeform shape (A, W, D, F). The window hides while you capture and comes back afterwards. Settings, the screenshots folder and Quit are in the ⋯ menu, and Settings opens in the same window.
- Freeform screenshots: draw around what to capture, and everything outside the shape is transparent. Also F in the Capture Bar.
- A delay for screenshots started from the window: 3, 5 or 10 seconds (T).
## [0.1.2](https://github.com/jakeklassen/shuttercrab/compare/v0.1.1...v0.1.2) - 2026-10-03

### Changed

- Starting Shuttercrab opens its window (for now, its settings), so it's on the taskbar as well as in the tray. Closing the window leaves Shuttercrab running in the tray; Settings in the tray menu opens it again. This replaces 0.1.1's separate taskbar button.
- Started at sign-in, Shuttercrab starts in the tray without opening its window.
## [0.1.1](https://github.com/jakeklassen/shuttercrab/compare/v0.1.0...v0.1.1) - 2026-10-03

### Added

- Screenshots and recordings work on every monitor. Area and Window cover all your screens, not only the one under the pointer, and Space switches them all between Area and Window.
- A taskbar button. Click it to open the Capture Bar. Turn it off in Settings, under General; the tray icon stays either way.
- The first time Shuttercrab starts, it opens Settings.

### Fixed

- In Window mode, clicking the hint at the top no longer captures the window behind it.
- "Where screenshots go" moved to the Screenshot page in Settings, beside the other screenshot settings.
## [0.1.0](https://github.com/jakeklassen/shuttercrab/releases/tag/v0.1.0) - 2026-10-02

The first release.

### Added

- Area, window and display screenshots, on the clipboard as PNG and saved to
  `Pictures\Shuttercrab`, with a thumbnail you can click to open or drag into
  another app.
- Colour that holds up with Windows HDR on: screenshots and recordings come
  out as you saw them, not washed out.
- Area and display recording to MP4, with pause, restart, discard and stop
  from the keyboard, a dashed border around the area, and an optional
  countdown.
- The Capture Bar (Ctrl+Alt+C), and hotkeys for everything, changeable in
  Settings.
- Shuttercrab's own bars, borders and thumbnails never appear in its
  captures.
- Updates from GitHub Releases, offered in the tray menu and never during a
  recording.
