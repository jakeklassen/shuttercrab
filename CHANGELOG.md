# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
