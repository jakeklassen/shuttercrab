# Draft issue for longbridge/gpui-kit: graphics memory kept after a window closes (Windows)

Not filed yet. Shuttercrab carries the fix in `vendor/gpui-pre-windows`
(see its `SHUTTERCRAB.md`).

---

**Title:** Windows: closed windows' render-target memory is never returned
(add `IDXGIDevice3::Trim` after a window closes)

## Summary

On Windows, every GPUI window allocates window-sized render targets: the
swap chain, the path intermediate texture and its 4× multisampled copy
(`PATH_MULTISAMPLE_COUNT`). When a window closes these are released, but
the graphics driver keeps their memory for reuse until the device is
trimmed, and `gpui-pre-windows` never calls `IDXGIDevice3::Trim`. An app
that opens and closes large windows keeps that memory for the rest of its
life. For a full-screen 4K window that is roughly 230 MB of graphics memory,
plus the driver's matching private (CPU-side) commitment.

## Environment

- gpui-kit 0.6.6 / gpui-pre-windows 0.3.6, and unchanged in 0.7.0 / 0.3.7
  (`src/` contains no call to `Trim`).
- Windows 11 (build 26300), NVIDIA GeForce RTX 4090, two 3840×2160 HDR
  monitors at 150%.

## Reproduction

1. Open a `WindowKind::PopUp` window covering a 4K monitor, draw one frame,
   and remove it (`window.remove_window()`).
2. Read the process's memory a few seconds later: private bytes
   (`GetProcessMemoryInfo`, `PrivateUsage`) and dedicated graphics memory
   (`IDXGIAdapter3::QueryVideoMemoryInfo`, `DXGI_MEMORY_SEGMENT_GROUP_LOCAL`).

Our app does this for every screenshot: a full-screen selection overlay that
shows the frozen screen.

## Measurements

| After closing a full-screen 4K window | Private | Graphics |
|---|---|---|
| Idle before | 85 MB | 39 MB |
| Without a trim | 415–446 MB | 345 MB |
| With the change below | 97–99 MB | 48 MB |

Repeating it does not grow the totals further: the driver reuses what it
kept. But the app then sits at about 4× its idle footprint indefinitely.

Opening and closing a small window 100 times showed the same pattern at a
smaller scale: about 120 MB of graphics and 250 MB of private memory kept
within the first 20 windows, then no further growth.

## Proposed fix

After a window is removed in `WindowsPlatformInner::close_one_window`, clear
the shared device context and trim the device. `ClearState` also resets the
rasterizer state that each renderer sets once when created
(`set_rasterizer_state`), so it is restored for the windows that remain open:

```rust
fn close_one_window(&self, target_window: HWND) -> bool {
    // ... existing code removing the handle ...
    lock.remove(index);
    self.trim_graphics_memory();
    lock.is_empty()
}

fn trim_graphics_memory(&self) {
    let devices = self.state.directx_devices.borrow();
    let Some(devices) = devices.as_ref() else {
        return;
    };
    unsafe {
        // Trim releases only memory not bound to the pipeline.
        devices.device_context.ClearState();
        devices.device_context.Flush();
    }
    // ClearState also reset the rasterizer state that renderers set
    // only once, when created; windows still open rely on it.
    directx_renderer::set_rasterizer_state(&devices.device, &devices.device_context)
        .log_err();
    if let Ok(dxgi) = devices
        .device
        .cast::<windows::Win32::Graphics::Dxgi::IDXGIDevice3>()
    {
        unsafe { dxgi.Trim() };
    }
}
```

and make `set_rasterizer_state` in `directx_renderer.rs` `pub(crate)`.

## Notes

- Trimming costs little, and happens only when a window closes.
- Alternatives worth considering upstream: allocate the path intermediate
  and its multisampled copy only when a scene contains paths (most windows
  draw none), which would also lower the memory of open windows; or move the
  rasterizer state into the per-frame setup (`pre_draw`), so a `ClearState`
  anywhere cannot break other windows.
- Zed's own GPUI may have the same behaviour; not checked.
