# Test matrix: Milestone 0

The Milestone 0 gate (PRD §31, §32.1, §32.2, §34.2) is **open**. The first
session (2026-09-27) passed every valid comparison but two of its captures
showed the wrong browser tab; see [Retaking captures](#retaking-captures).
Milestone 1 starts only after the gate passes. [COLOR_PIPELINE.md](COLOR_PIPELINE.md) describes
what is being tested.

## Automated checks

From the repository root, in PowerShell:

```powershell
mise exec -- cargo fmt --all -- --check
mise exec -- cargo clippy --all-targets -- -D warnings
mise exec -- cargo test
```

`cargo test` includes GPU golden tests. They run on WARP (software, so they
work anywhere) and on the hardware adapter when there is one.

## The HDR-off / HDR-on gate

About ten minutes. The `gate` command guides the session: it tells you what to
set, checks with Windows that the setting took effect before capturing, and
writes one `summary.md` at the end.

### What it captures

Two scenes, each in four display states, eight captures in all:

| State | Windows setting | Role |
|---|---|---|
| `hdr-off` | HDR off | the reference: Windows' own 8-bit desktop |
| `hdr-low` | HDR on, SDR content brightness **10** | |
| `hdr-mid` | HDR on, SDR content brightness **50** | |
| `hdr-high` | HDR on, SDR content brightness **100** | |

| Scene | What to show |
|---|---|
| `fixture` | `fixtures/sdr-reference.html` full screen: neutrals, every grey/R/G/B code, saturated and UI colors, gradients, light and dark code, text |
| `mixed` | the same page with `?hdr`: adds a BT.2020 PQ test image in the bottom-right corner |

The low setting is 10, not 0, on purpose. If the slider maps 0 to 80 nits, `S`
is 1 and a bug that ignores `S` would pass there. The gate reads the real white
level at each step and refuses to continue if two states share one.

### Before you start

1. Build: `mise exec -- cargo build --release -p capture-spike`
2. Night light off. Keep the same resolution and scaling (currently 3840×2160 at
   150%) throughout.
3. Open the two scenes as two tabs of one Edge window. Edge is the browser the
   HDR test image was verified in. From PowerShell in the repository root:

   ```powershell
   $page = (Resolve-Path fixtures\sdr-reference.html).Path -replace '\\','/'
   Start-Process msedge -ArgumentList "file:///$page", "file:///${page}?hdr"
   ```

   Set zoom to 100% (Ctrl+0), then press **F11** for full screen. Ctrl+Tab
   switches tabs in full screen. Nothing on the page moves, so it looks the
   same every time you come back to it.
4. Park the pointer at the screen's edge. It is not captured, but hover states
   are.

### Run it

In a terminal in the repository root:

```powershell
.\target\release\capture-spike.exe gate
```

At each step it prints what to set, and waits for Enter:

- **HDR on/off:** Settings > System > Display > HDR > *Use HDR*, or press
  Win+Alt+B.
- **SDR content brightness:** Settings > System > Display > HDR > *SDR content
  brightness*.

For each scene it prints `Show ...`: press Enter, switch to Edge (Alt+Tab),
select the scene's tab, and leave it on screen. The capture happens 5 seconds
later, and the terminal beeps when it is done. Take longer with `--delay 8`.
If a capture fails it says why and asks again. After each capture it also
checks the right tab was showing: `fixture` must hold no HDR content, `mixed`
must (except at brightness 100, where a panel may have no headroom left), and
no two scenes of a state may be identical. If one looks wrong it offers to
retake it. Type `q` at any prompt to stop.

When the four states are done it writes
`captures\gate-<timestamp>\summary.md` and prints it. To re-run only the
analysis: `capture-spike gate-report captures\gate-<timestamp>`.

### Reading the result

The verdict at the end is **PASS** only when all of these hold:

- the session is valid: reference not in HDR, three distinct SDR white levels,
  each scene showing what its name says (otherwise the verdict is
  **UNDECIDED**);
- every HDR state matches the reference, per scene: mean ΔE00 ≤ 0.5, 99th
  percentile ≤ 1.0, and no 64×64 block averaging over 1.0;
- the three HDR states match each other by the same measure.

In a frame with HDR content, the region of that content is left out: tiles
where a quarter of the pixels are not SDR content, grown by one tile and filled
to each area's bounding box. With HDR off, Edge tone maps the image itself,
which a capture cannot reproduce. The captures table gives the region's
bounds, so you can check it is the image and nothing else. Without HDR content
nothing is left out.

The report re-converts HDR captures from their saved FP16 frames with the
current transform, so an old session can be re-analysed after a fix.

The mean and percentile thresholds were fixed before any measurement. The
block limit was added after the first session, where a wrong 768×384 region
passed the other two because it covered under 1% of the frame. The expected
result is an exact match: the model predicts identical codes, and SDR content measured
100% on the code grid. A systematic ±1-code tint fails on purpose: it is a
model error even though it is below one just-noticeable difference.

The summary also holds:

- Windows' own 8-bit capture compared with the reference. It is the naive
  result and is expected to fail badly; it shows what the gate would catch.
- **How Windows placed SDR codes**, measured against the reference: the median
  FP16 value for each grey code, next to what piecewise sRGB and gamma 2.2
  predict. If this disagrees with the model, the transform is wrong; if it
  agrees and a comparison still fails, the application drew different codes in
  the two modes.
- **Highlights** in the mixed scene: how many pixels exceed SDR white, and how
  many distinct output colors they keep with the shoulder versus clipping.

`diffs\` holds a heatmap per comparison: the reference dimmed, red where it
differs (full red at ΔE00 4), blue where left out. A passing heatmap is plain
dim grey, with blue only on the HDR image.

### What to send back

1. The whole `summary.md`.
2. A look, not a measurement: open `hdr-mid\mixed\capture.png` and
   `hdr-off\mixed\capture.png` at 100% and say whether the page looks the
   same, and whether the HDR image's bright steps and textured strips show
   detail or flat white.
3. Only if something fails: the `diffs\*.png` heatmaps of the failing lines.
   They show the test page only.

Keep the rest (`source.fp16` is 66 MB per capture). Those files allow a
`transfer`, `probe` or `convert` re-analysis without capturing again.

### Retaking captures

To retake some states of a session, point `--out` at it and name the states.
The rest of the session is kept, and the report covers all of it:

```powershell
.	argeteleasepture-spike.exe gate --out capturesgate-20260927-112049 --states hdr-low,hdr-mid
```

That is what the first session needs: at `hdr-low` the `mixed` capture showed
the plain tab, and at `hdr-mid` the `fixture` capture showed the `?hdr` tab.

## Manual tools

| Command | Use |
|---|---|
| `capture-spike list` | Monitors, mode, SDR white level, `S` |
| `capture-spike capture [--monitor DISPLAY1] [--delay 5]` | One capture into `captures\` |
| `capture-spike compare A.png B.png [--source B.fp16] [--roi X,Y,W,H] [--heatmap D.png]` | ΔE00 comparison; `--source` leaves out the HDR content region |
| `capture-spike transfer REF.png SRC.fp16` | Measured SDR transfer against an HDR-off reference |
| `capture-spike fit SRC.fp16 [--roi …] [--map M.png]` | The same without a reference: how close values sit to 8-bit codes |
| `capture-spike probe SRC.fp16 X,Y,W,H` | Exact values over a region |
| `capture-spike convert SRC.fp16 OUT.png [--highlights clip]` | Re-run the transform on a saved frame |

Coordinates are physical pixels from the captured monitor's top-left corner.

## Matrix

| Case (PRD) | Status | Evidence |
|---|---|---|
| Monitor enumeration, Advanced Color / HDR state, SDR white level (§8.2–8.4) | Passed locally | `list`: 4K HDR, level 3000 (240 nits), mode from `ADVANCED_COLOR_INFO_2` |
| WGC FP16 capture → shader → sRGB PNG (§8.5, §9.2) | Passed locally | 4K in 68 ms + 9.5 ms |
| Shader arithmetic (§33 golden tests) | Passed | WARP and RTX 4090 |
| Windows stores SDR as `S × sRGB⁻¹(code)` | Passed | 100.00% on the code grid (terminal); against HDR-off references at `S` = 1.5, 3.5, 6: within 0.17 code, white / `S` = 1.00000 |
| SDR content untouched beside HDR content (§9.6) | Passed locally | Mixed scene: only the HDR image changes |
| Highlight texture kept (§9.6, §34.2) | Passed locally, needs your eyes | Mixed scene: steps and strips separate; clip flattens them |
| **HDR off vs on, fixture (§32.1, §34.2)** | **Undecided: retake `hdr-mid` fixture** | Identical at `S` = 1.5 and 6; ≤ 1 code at 3.5 (from the `?hdr` capture) |
| **SDR brightness 10 / 50 / 100 invariance (§32.2)** | **Passed in the first session; confirm on retake** | All pairs pass; slider = 80 + 4 × value nits |
| Light-theme browser (§31) | Passed at `S` = 1.5, 6; pending retake at 3.5 | Fixture |
| Light-theme IDE, dark-theme IDE (§31) | As the fixture (its code blocks); optionally add `--scenes fixture,mixed,vscode-light,vscode-dark` | |
| Saturated colors (§31) | Passed at `S` = 1.5, 6; with HDR on screen after the v3 cross-talk fix | Fixture |
| HDR content beside SDR UI (§31) | Passed at `S` = 3.5 and 6; pending retake at 1.5 | Mixed scene |
| SDR monitor path (§9.5) | Passed | HDR off: FP16 path and 8-bit capture identical |
| Advanced Color SDR (WCG) hardware | Not available | `S = 1` from the SDK's description |
| 125/150/200% scaling, second monitor (§32.3) | Milestone 5 | |
