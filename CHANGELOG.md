# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
