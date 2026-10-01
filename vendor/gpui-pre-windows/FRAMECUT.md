# Framecut's copy of gpui-pre-windows

This is `gpui-pre-windows` 0.3.7 as published on crates.io (GPUI's Windows
platform layer, from [longbridge/gpui-kit](https://github.com/longbridge/gpui-kit),
Apache-2.0, see `LICENSE-APACHE`), used through `[patch.crates-io]` in the
workspace `Cargo.toml`, with one change.

## The change

`src/platform.rs`: after a window closes (`close_one_window`),
`trim_graphics_memory` clears the device context's state, restores the
rasterizer state GPUI renderers set once at creation, and calls
`IDXGIDevice3::Trim`. `src/directx_renderer.rs`: `set_rasterizer_state` is
`pub(crate)` so the platform can restore it.

Why: every GPUI window has window-sized render targets. When a window
closes they are released, but the graphics driver keeps their memory for
reuse until the device is trimmed, and GPUI never trims. Framecut opens and
closes a full-screen 4K window for every screenshot (the selection
overlay), so each screenshot left about 300 MB of graphics memory and
about 330 MB of private memory behind. With the change: 97–99 MB private and
48 MB graphics after a screenshot, against 85 MB and 39 MB at idle
(docs/TEST_MATRIX.md, Milestone 5). The upstream report is drafted in
`docs/upstream/gpui-trim-after-window-close.md`.

## Updating

When gpui-kit moves to a new `gpui-pre-windows`:

1. Check whether upstream trims after closing windows (search its
   `src/` for `Trim`). If it does, delete this folder and the
   `[patch.crates-io]` entry.
2. Otherwise, replace this folder with the new version from crates.io
   (`~/.cargo/registry/src/*/gpui-pre-windows-<version>`), commit it
   unchanged, then reapply the change above as its own commit, and
   update the version at the top of this file.
3. Rerun the memory check in docs/TEST_MATRIX.md (Milestone 5).
