# capture-spike

Shuttercrab's Milestone 0 executable: capture one monitor through
Windows.Graphics.Capture in FP16, convert it to SDR on the GPU, and measure the
result against an HDR-off reference. Run it with no arguments for usage.

- The transform: [docs/COLOR_PIPELINE.md](../../docs/COLOR_PIPELINE.md)
- How the tools fit into testing: [docs/TESTING.md](../../docs/TESTING.md)
- The gate's procedure and the manual tools: [docs/history/TEST_MATRIX.md](../../docs/history/TEST_MATRIX.md)

The pipeline (monitors, WGC capture, the transform and its CPU reference)
lives in [`shuttercrab-capture`](../shuttercrab-capture). This crate adds `snapshot` (one capture to disk), `analysis` (ΔE00, transfer and
code-grid measurements), `gate` (the guided session), `fixture` (the HDR test
image).
