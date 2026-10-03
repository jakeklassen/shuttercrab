# Shuttercrab's copy of gpui-pre-windows

This is `gpui-pre-windows` 0.3.7 as published on crates.io (GPUI's Windows
platform layer, from [longbridge/gpui-kit](https://github.com/longbridge/gpui-kit),
Apache-2.0, see `LICENSE-APACHE`), used through `[patch.crates-io]` in the
workspace `Cargo.toml`, with three changes.

## Path textures only when a window draws paths

`src/directx_renderer.rs`: the path intermediate textures (one
window-sized, one window-sized and 4× multisampled) are made the first
time a scene has paths (`PathTextures`), and dropped on resize, rather than
made with every window. Why: a full-screen 4K window spent about 166 MB on
them whether it drew a path or not, and Shuttercrab's selection overlay
covers every monitor. Upstream could take this as it is.

## Two swap chain buffers

`src/directx_renderer.rs`: `BUFFER_COUNT` is 2, not 3. Each buffer is the
window's size (33 MB at 4K); two is the least a flip-model swap chain
allows and keeps up with everything Shuttercrab draws.

Together with Shuttercrab's own changes (the frozen screen held once, the
Freeform outline drawn without paths), the overlay's peak with two 4K
monitors went from about 770 MB private to about 300–340 MB
(docs/TEST_MATRIX.md, "The overlay's memory peak").

## Trimming after a window closes

`src/platform.rs`: after a window closes (`close_one_window`),
`trim_graphics_memory` clears the device context's state, restores the
rasterizer state GPUI renderers set once at creation, and calls
`IDXGIDevice3::Trim`. `src/directx_renderer.rs`: `set_rasterizer_state` is
`pub(crate)` so the platform can restore it.

Why: every GPUI window has window-sized render targets. When a window
closes they are released, but the graphics driver keeps their memory for
reuse until the device is trimmed, and GPUI never trims. Shuttercrab opens and
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
   unchanged, then reapply each change above as its own commit (skipping
   any upstream now makes), and update the version at the top of this
   file.
3. Rerun the memory check in docs/TEST_MATRIX.md (Milestone 5).
