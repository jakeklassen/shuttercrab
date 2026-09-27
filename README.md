# Framecut

A Windows 11 screenshot and screen-recording utility, written in Rust with
[GPUI Kit](https://gpui-kit.com). It works like Snipping Tool (one hotkey, drag,
paste) and produces SDR output that looks right when Windows HDR is enabled.

The specification is [docs/PRD.md](docs/PRD.md). Work starts with Milestone 0:
prove HDR-to-SDR color correctness in a standalone capture spike before any
product UI.

## Setup

Rust is pinned with [mise](https://mise.jdx.dev) in `mise.toml`.

```powershell
mise install
```
