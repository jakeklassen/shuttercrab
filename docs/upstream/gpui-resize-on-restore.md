# Draft issue for longbridge/gpui-kit: a window restored at a new size is drawn at the old one (Windows)

Not filed yet. Shuttercrab carries the fix in `vendor/gpui-pre-windows`
(see its `SHUTTERCRAB.md`).

---

**Title:** Windows: restoring a minimized window at another size leaves part
of it see-through (renderer not resized on restore)

## Summary

`handle_size_msg` in `src/events.rs` skips the renderer when a minimized
window is restored (`should_resize_renderer` is false whenever
`restore_from_minimized` was set), but still reports the new size to the
window. If the window comes back at another size than it was minimized at,
GPUI lays it out at the new size and draws it into a swap chain of the old
size. The part of the window outside the old size shows whatever is behind
it.

The comment there says the swap chain is recreated on restore "via
`update_drawable_size_even_if_unchanged`", but no such function exists in
`gpui-pre-windows`, and nothing else resizes the renderer.

A second, smaller problem in the same function: each `SIZE_MINIMIZED` does
`restore_from_minimized.set(request_frame.take())`. If a minimized window
gets `SIZE_MINIMIZED` twice (for example when it is moved or sized while
minimized), the second one takes `None` from `request_frame` and replaces
the kept callback with it, so the window has no frame callback after it is
restored and never draws again.

## Environment

- gpui-kit 0.8.1 / gpui-pre-windows 0.3.10.
- Windows 11 (build 26300), two 3840×2160 HDR monitors.

## Reproduction

1. Open a normal window and show it at 1000×600.
2. Minimize it (`ShowWindow(hwnd, SW_MINIMIZE)`).
3. While it is minimized, change its restored size with
   `SetWindowPlacement`: `showCmd = SW_SHOWMINIMIZED`, `rcNormalPosition`
   1600×800.
4. Restore it (`ShowWindow(hwnd, SW_RESTORE)`).

The window is 1600×800, laid out at that size, but drawn only as far as the
old size; the rest is see-through. We first saw it in the app itself,
after restoring a window that had been minimized; what changed its size
while minimized there is not known.

## Proposed fix

`DirectXRenderer::resize` already returns early when the size is unchanged,
so the renderer can be resized on every `WM_SIZE` but a minimize, and only
an actual callback is kept on minimize:

```rust
fn handle_size_msg(self: &Rc<Self>, wparam: WPARAM, lparam: LPARAM) -> Option<isize> {
    self.report_visibility();

    if wparam.0 == SIZE_MINIMIZED as usize {
        if let Some(request_frame) = self.state.callbacks.request_frame.take() {
            self.state.restore_from_minimized.set(Some(request_frame));
        }
        return Some(0);
    }

    let width = lparam.loword().max(1) as i32;
    let height = lparam.hiword().max(1) as i32;
    let new_size = size(DevicePixels(width), DevicePixels(height));

    let scale_factor = self.state.scale_factor.get();
    if let Some(restore_from_minimized) = self.state.restore_from_minimized.take() {
        self.state
            .callbacks
            .request_frame
            .set(Some(restore_from_minimized));
    }

    self.handle_size_change(new_size, scale_factor);
    Some(0)
}
```

`handle_size_change` then no longer needs its `should_resize_renderer`
parameter; its other caller (`handle_dpi_changed_msg`) already passes
`true`.

## Notes

- Tested with the steps above: without the change the bottom of the window
  was see-through; with it the whole window drew.
- Zed's own GPUI has the same code in `handle_size_msg`; not checked
  whether it behaves the same there.
