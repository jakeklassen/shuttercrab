# Framecut

A Windows 11 screenshot and screen-recording utility, written in Rust with
[GPUI Kit](https://gpui-kit.com). It works like Snipping Tool (one hotkey, drag,
paste) and produces SDR output that looks right when Windows HDR is enabled.

The specification is [docs/PRD.md](docs/PRD.md).

## Status: Milestone 0

Work starts by proving HDR-to-SDR color correctness in a standalone executable,
[`capture-spike`](crates/capture-spike), before any product UI. It enumerates
monitors, reports each one's Advanced Color and HDR state and SDR white level,
captures a frame with Windows.Graphics.Capture in FP16, converts it to SDR with
a GPU shader, and writes a PNG.

- [docs/COLOR_PIPELINE.md](docs/COLOR_PIPELINE.md): the color transform, its
  equations, and the measurements behind it.
- [docs/TEST_MATRIX.md](docs/TEST_MATRIX.md): the HDR-off / HDR-on gate that
  decides Milestone 0, and how to run it.

The gate has not been run yet.

## Setup

Rust is pinned with [mise](https://mise.jdx.dev) in `mise.toml`.

```powershell
mise install
mise exec -- cargo build --release -p capture-spike
.\target\release\capture-spike.exe list
```

Checks:

```powershell
mise exec -- cargo clippy --all-targets -- -D warnings
mise exec -- cargo test
```
