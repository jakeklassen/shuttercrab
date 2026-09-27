# capture-spike

Framecut's Milestone 0 executable: capture one monitor through
Windows.Graphics.Capture in FP16, convert it to SDR on the GPU, and measure the
result against an HDR-off reference. Run it with no arguments for usage.

- The transform: [docs/COLOR_PIPELINE.md](../../docs/COLOR_PIPELINE.md)
- The gate and the manual tools: [docs/TEST_MATRIX.md](../../docs/TEST_MATRIX.md)

The pipeline (monitors, WGC capture, the transform and its CPU reference)
lives in [`framecut-capture`](../framecut-capture). This crate adds `snapshot` (one capture to disk), `analysis` (ΔE00, transfer and
code-grid measurements), `gate` (the guided session), `fixture` (the HDR test
image).
