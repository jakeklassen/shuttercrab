# Color pipeline

Status: **the Milestone 0 gate passed on 2026-09-27** (session
`gate-20260927-112049`, 12 of 12 comparisons; see [Evidence](#evidence)),
pending the project owner's confirmation. Transform version:
`sdr-exact-hdr-regions-v4`, which re-passes the same session (see
[Evidence](#evidence)).

This is the color decision record the PRD requires (§37, items 3 and 4): what
Framecut does to turn a Windows desktop capture into an SDR PNG, why, and the
evidence behind it. The code that implements it is
[`shaders/hdr_to_sdr.hlsl`](../crates/framecut-capture/shaders/hdr_to_sdr.hlsl),
mirrored step for step by the CPU reference in
[`src/color.rs`](../crates/framecut-capture/src/color.rs). Tests hold the two to
each other and to known values.

## The contract

For ordinary SDR content (UI, text, web pages, code) shown on an HDR-enabled
monitor, Framecut's PNG must match a capture of the same content with HDR off
(PRD §9.1), whatever the SDR content brightness slider is set to (§32.2).
Content brighter than SDR white must compress gracefully instead of washing
out the image (§9.6).

## What Windows gives us

**Capture source.** Windows.Graphics.Capture, `CreateForMonitor`, a
free-threaded frame pool in `R16G16B16A16_FLOAT`. The frame is the
compositor's own surface: **linear scRGB**, i.e. BT.709 primaries, D65 white,
linear light, values allowed below 0 (colors outside sRGB) and above 1. It is
not PQ and not BT.2020, whatever the monitor's output signal is. Nothing is
reduced to 8 bits before the shader.

**Display mode.** DisplayConfig's `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2`
(Windows 11 24H2 and later; windows 0.62 predates it, so the struct is defined
locally from the 10.0.26100 SDK header) reports the active mode. The SDK
header documents the three modes:

| Mode | SDK description | scRGB 1.0 means | `S` |
|---|---|---|---|
| SDR | RGB888 composition, display-referred | (no scRGB stage) | 8-bit path |
| WCG (Advanced Color SDR) | FP16 scRGB composition, display-referred luminance | SDR white | 1 |
| HDR | FP16 scRGB composition, scene-referred luminance | 80 nits | `SDRWhiteLevel / 1000` |

On older builds the legacy query only says whether Advanced Color is on; HDR is
then told apart from WCG by the output's DXGI color space
(`RGB_FULL_G2084_NONE_P2020`). `list` and each report show the source used and
any disagreement between the two.

**SDR white.** `DISPLAYCONFIG_SDR_WHITE_LEVEL.SDRWhiteLevel` is a fixed-point
multiple of 80 nits (header: "SDRWhiteLevel in nits = (SDRWhiteLevel / 1000) ×
80"). In HDR mode SDR content is placed at `S = SDRWhiteLevel / 1000` in scRGB.
It is queried on every capture, and again after it, and the capture is refused
if the display state changed in between. It is never assumed: an HDR display
that reports no level is an error.

**SDR monitors** (neither HDR nor WCG) are captured in `B8G8R8A8_UNORM`, which
is the desktop exactly. No tone mapping runs (PRD §9.5). The FP16 path is still
measured against it in the report, as a self-check.

## The model: how Windows stores SDR content

An 8-bit SDR code `c` becomes, in the HDR compositor,

```text
C = S × D(c / 255)

D(e) = e / 12.92                     e ≤ 0.04045
       ((e + 0.055) / 1.055)^2.4     otherwise        (IEC 61966-2-1, piecewise sRGB)
```

That is Framecut's working hypothesis, and it is measured, not assumed (see
[Evidence](#evidence)). On a real 4K HDR capture at `SDRWhiteLevel` 3000, **all
9.5 million channel values of ordinary SDR app content lie within 0.1 of an
integer code** under this model (mean distance 0.008 code). Gamma 2.2 fits far
worse, and content that is not 8-bit SDR (video) fits neither better than
chance. Windows does *not* use gamma 2.2 for SDR content, and `S` is exactly
the queried level.

The model makes the inverse exact: `E(C / S) = E(D(c/255)) = c/255`, where `E`
is the sRGB encoding. Dividing by `S` has to happen in linear light, before
encoding. Dividing already-encoded 8-bit values cannot undo it.

## The transform

For each pixel of scRGB `C`:

1. **Normalize.** `N = C / S`. NaN becomes 0; values are clamped to ±65504
   (FP16's range).

2. **Gamut.** A negative channel means a color outside sRGB (wide-gamut
   content). Desaturate it toward its own luminance, just far enough to make the
   smallest channel 0. Luminance and hue angle are kept:

   ```text
   Y  = 0.2126 N.r + 0.7152 N.g + 0.0722 N.b
   if min(N) < 0:   N ← 0 if Y ≤ 0,   else   N ← Y + (Y / (Y − min N)) (N − Y)
   ```

   The prior spike clipped negative channels to 0 instead, which lightens and
   shifts the color.

3. **Is it SDR content?** A channel `n` of a pixel whose peak channel is `t`
   is *on the code grid* when it is negligible (`|n| < 0.005`, below code
   ≈ 18), or when `0 ≤ n ≤ 1 + ε` and, with `c = round(255 E(n))`, either
   `|255 E(n) − c| ≤ 0.25` or `|n − D(c/255)| ≤ 0.001 t`. A pixel is
   **SDR content** when all channels of it and its eight neighbours are on
   the grid.

4. **Where is HDR content?** Per 16×16 tile the GPU counts pixels that are
   not SDR content, counts pixels above SDR white (`max(N) > 1 + ε`), keeps
   the bounding box of the non-SDR pixels, and the tile's peak `max(N)`. A
   tile *qualifies* when it holds a pixel above SDR white or when at least a
   quarter of it is not SDR content. On the CPU, qualifying tiles are grown by
   one tile and joined into 4-connected areas; an area with no pixel above
   SDR white is dropped (SDR video, compositor effects). Each remaining area
   becomes an **HDR region**: the bounding box of its non-SDR pixels, trimmed
   by one pixel (the ring the 3×3 rule adds around any non-SDR content), with
   `P` = the area's peak. At most 32 regions; more are merged into one.

5. **Tone map HDR regions.** Inside a region, with `m = max(N)`:

   ```text
   g    = 1 / √P                                            gain: dims the content to leave room
   x    = g · m
   f(m) = x                                                 x ≤ 0.45
   f(m) = 0.45 + 0.55 · a (1 + a/A²) / (1 + a)              x > 0.45,  a = (x − 0.45)/0.55,  A = (√P − 0.45)/0.55
   N    ← N · f(m) / m
   ```

   `f` dims the region's content by `g`, stays linear up to 0.45, then rolls
   off with an extended-Reinhard shoulder that reaches exactly 1 at the
   region's peak (`g · P = √P`). Scaling all three channels by the same factor
   keeps hue and channel ratios. Everything outside regions, SDR content or
   not, is left as captured.

6. **Into the cube.** `N ← N / max(1, max(N))`. A no-op except for values
   within `ε` of white, stray extended pixels outside any region, and
   everything in `--highlights clip` mode.

7. **Encode.** `code = floor(255 · E(N) + 0.5)` per channel, alpha 255. The
   shader writes the packed bytes itself (into `R32_UINT`) rather than trust
   UNORM conversion, whose rounding D3D11 only bounds to 0.6 ULP.

Output is an 8-bit RGBA PNG with an sRGB chunk (plus the gAMA/cHRM fallbacks),
and no HDR metadata.

### Constants

| Constant | Value | Why |
|---|---|---|
| `ε` (`EXTENDED_EPSILON`) | 1/256 | SDR white stored in FP16 can read a hair above 1 (FP16 step 2⁻¹¹ relative); it must not count as HDR |
| `CODE_TOLERANCE` | 0.25 code | Near white one FP16 step is ≈ 0.073 code. Edge compositing SDR content in its own FP16 pipeline lands up to 2 steps off. This allows ≈ 3 |
| `NEGLIGIBLE` | 0.005 | Edge leaks ≈ +0.0001 into channels that should be 0 (code 0.4 of red in pure green); channels this dark never matter |
| `CROSS_TALK` | 0.001 × peak | Edge's FP16 color conversion moves a dim channel beside a bright one by up to ≈ 0.0004 × the bright one: red codes 22–61 on a green gradient sat 0.27–0.37 code off the grid |
| `TILE` | 16 px | Grain of the HDR-region analysis |
| `REGION_SHARE` | 1/4 | A tile a quarter non-SDR is HDR content even without highlights (the dark parts of an HDR image) |
| `TONE_KNEE` | 0.45 | Where the curve leaves linear; fitted with `g = 1/√P` to Edge's own HDR-off rendering |
| `MAX_REGIONS` | 32 | Constant-buffer size |

What the curve does: at `P` = 3.79 (SDR white 120 nits on this panel), HDR
content at SDR white lands at code ≈ 190, and the 250–500-nit steps of the test
image at 202–232, matching Edge's own rendering (201–232) within 4 codes. The
peak is 255. At `P` = 1.06 (little headroom) the gain is 0.97 and the content
is barely touched.

### Why it is built this way

- **Exactness first.** Any per-pixel curve that leaves SDR white at 1 and has
  output bounded by 1 must send everything above 1 to 1. That is the prior
  spike's constraint, and why it turned neutral highlights flat white. Keeping
  highlight detail therefore needs to know *which* pixels are HDR content.
- **Classification beats proximity.** An early version applied a shoulder near
  extended pixels, using a tile map. On a live capture it dimmed the white page
  around an HDR image to 248, in tile-shaped blocks. The grid test uses what
  Windows guarantees about SDR content instead.
- **Regions, not pixels, get the curve.** v2 and v3 applied a shoulder to each
  non-SDR pixel. HDR content that happens to sit on the grid (Edge puts the
  203-nit reference white of an HDR image exactly at SDR white) then stayed at
  255 while brighter content beside it was compressed below it: a visible
  inversion. v4 finds each HDR area and maps all of it with one curve.
- **Why this curve.** Capture tools disagree (see the research summary in
  [TEST_MATRIX.md](TEST_MATRIX.md#hdr-content-benchmark)): Snipping Tool's
  corrector greys all white UI to 224, OBS to ≈ 191, NVIDIA and ShareX do not
  tone map. Broadcast practice (ITU-R BT.2446) puts HDR reference white at
  86–96% of SDR; browsers (Chromium/Skia) at half brightness, leaving room for
  highlights. The owner compared v3, three lighter log-spread curves, and
  Edge-like curves side by side on real captures, and chose the Edge-like one:
  every step of the test image stays distinguishable. v3 spread everything
  above SDR white over ≈ 6 codes; v4 spreads it over ≈ 65.
- **The gain from the peak is a first choice, not a settled one.** A second
  review (Codex) and the research both flag that one bright pixel sets the
  exposure of its whole region, so cropping or a spark can change it. It is
  per region, which limits the reach. A steadier anchor is follow-up work,
  to be judged on real HDR video in Milestone 1.
- **Nine pixels, not one.** Non-SDR content lands on the grid by chance about
  half the time per tested channel. Requiring the 3×3 neighbourhood makes a
  chance match need nine coincidences even for saturated content (≈ 0.2%), and
  ~27 for neutral content; regions are built from tiles, so scattered chance
  matches inside HDR content do not break a region.
- **Tolerances come from measurements.** Each was widened only after a real
  capture showed SDR content off the grid, and only as far as that cause
  (FP16 rounding, zero-channel residue, cross-talk) explains. The regression
  tests hold the measured pixel values.
- **No scene-dependent exposure for SDR content.** Nothing outside HDR regions
  depends on the rest of the frame.

### Ported from the prior spike, and changed

Kept: WGC `CreateForMonitor` with a free-threaded FP16 pool on the output's own
adapter, D3D11 multithread protection, the DXGI ↔ DisplayConfig join by GDI
source name, refusal of cloned paths, re-enumeration after capture,
`SDRWhiteLevel / 1000` in HDR mode only, and explicit session/pool closing.

Changed:

| Prior spike | Now | Why |
|---|---|---|
| Neutral highlights clip to flat white | HDR regions tone mapped | Highlight texture survives; SDR content still exact |
| Negative channels clipped to 0 | Luminance-preserving desaturation | Keeps luminance and hue of wide-gamut colors |
| HDR = DXGI PQ color space | `ADVANCED_COLOR_INFO_2` active mode, DXGI as fallback and cross-check | Direct answer from Windows; distinguishes WCG |
| FP16 path for SDR monitors too | 8-bit capture on SDR monitors | PRD §9.5; also makes the HDR-off reference the true desktop |
| UNORM output conversion | Explicit rounding into `R32_UINT` | Deterministic, testable to the code |
| Validation: synthetic ramps only | Plus measurements on real captures (`fit`, `transfer`) and a guided gate | The model is now measured |

## Shader passes

All passes are D3D11 compute (`cs_5_0`), compiled at start-up with
`D3DCompile` from the embedded HLSL.

| Pass | Reads | Writes |
|---|---|---|
| `classify` | source | on-grid flag per pixel (`R8_UNORM`) |
| `tile_stats` | source, flags | per tile: peak, counts, non-SDR bounding box (structured buffer) |
| CPU: `find_regions` | tile stats (≈ 32 KB at 4K) | up to 32 HDR regions (constant buffer) |
| `convert` | source, regions | packed RGBA8 (`R32_UINT`) |

The frame stays on the GPU; only the tile summary and the final RGBA8 result
are read back.

## Evidence

All on 2026-09-27: Windows 11 build 26200, NVIDIA GeForce RTX 4090, LG
UltraGear+ 3840×2160 at 150%, HDR on, `SDRWhiteLevel` 3000 (240 nits, `S` = 3),
DXGI peak 456 nits.

| What | Result |
|---|---|
| Unit tests (CPU) | 37 pass: sRGB transfer, every code at seven white levels, hand-derived scRGB → code values, gamut mapping, grid classification (including pixels measured from Edge), HDR-region detection, the HDR curve against Edge's measured codes, CIEDE2000 against Sharma et al.'s published pairs, the gate's region and block checks |
| GPU golden tests | Pass on WARP and the RTX 4090: every grey/R/G/B code comes back exactly at `S` ∈ {1, 1.25, 1.5, 2.4, 3, 3.5, 6}; mixed scenes match the CPU reference within one code; SDR codes touching an HDR ramp stay exact |
| SDR white in a real capture | Frame peak exactly 1.000 × `S` on a desktop without HDR content |
| Model fit, SDR app region (terminal) | piecewise sRGB: 100.00% of 9,475,648 channel values within 0.1 code, mean 0.008; gamma 2.2: 65.93%, mean 0.118 |
| Model fit, playing SDR video | 20.44% vs 19.23%: chance, as expected for non-code content |
| Model fit, Edge page with HDR content | page white is 1–2 FP16 steps below `S`; zero channels carry ±0.0001 residue; led to the tolerance and negligible-channel rule above |
| Mixed scene (fixture `?hdr` in Edge) | Edge renders the PQ test image as HDR, limited by the panel's peak (≈ 456 nits); v4 finds exactly one HDR region, (3000, 1704)–(3767, 2087): the 768×384 image to the pixel |
| Timing (release, 4K) | FP16 capture 68 ms; v3's passes + readback 9.5 ms. v4 adds a tile-stats pass and a ≈ 1 MB readback; re-measure on the next live capture |

### First gate session (2026-09-27, `gate-20260927-112049`)

Captured with transform v2 and re-analysed from the saved FP16 frames with v3.
HDR off was plain SDR mode (not WCG). The SDR content brightness slider maps
linearly to 80 + 4 × slider nits: 10, 50 and 100 read `SDRWhiteLevel` 1500,
3500 and 6000 (`S` = 1.5, 3.5, 6).

| What | Result |
|---|---|
| SDR monitor path | FP16 capture + shader vs Windows' 8-bit capture: identical (max 0 codes) |
| Fixture, `S` = 1.5 and 6, vs HDR off | Identical: 8,294,400 px, max difference 0 codes |
| Fixture content at `S` = 3.5, HDR image on screen, vs HDR off | Max 1 code (rounding) outside the HDR image's region |
| Mixed page at `S` = 6, vs HDR off | Max 1 code outside the HDR image's region |
| Brightness invariance, all pairs | Pass |
| Measured transfer at every `S`, against the reference | piecewise sRGB within 0.17 code at every grey code; measured white / `S` = 1.00000 (0.99935, one FP16 step, in Edge's HDR path); gamma 2.2 off by 8.5 codes |
| v2 → v3 | v2 dimmed 748 px of saturated green in Edge (≤ 5 codes) when HDR content was on screen: red cross-talk from Edge's conversion failed the grid test. v3's cross-talk tolerance fixes it; the pixels are now regression tests |
| Retakes | `hdr-low/mixed` had shown the plain tab and `hdr-mid/fixture` the `?hdr` tab (the gate now detects both); a retake of `hdr-mid/fixture` then caught a notification. Final captures: all as labelled |
| Final verdict | **PASS**: fixture identical to HDR off (0 codes, every pixel) at `S` = 1.5, 3.5 and 6; mixed scene ≤ 1 code outside the HDR image at all three; brightness invariance passes for every pair |
| Highlight policy (owner's review) | v3's shoulder was judged "a little better" than clipping but lost detail visible on the live HDR tab. After a benchmark against Snipping Tool and Edge's own HDR-off rendering, the owner chose the Edge-like curve (v4): "we can always refine" once real UI and content flow |
| v4 re-analysis of the session | **PASS** again, 12 of 12: SDR content unchanged. HDR image vs Edge's own HDR-off rendering: mean ΔE00 0.51 / 1.07 / 0.21 at `S` = 1.5 / 3.5 / 6 (v3: 7.29 / 5.32 / 0.95). Distinct output colors on the extended pixels at 3.79× headroom: 106 (v3: 29; clipping: 8) |

## Known limits and expected differences

- **Compositor blending.** With HDR on, DWM blends translucent surfaces
  (shadows, Mica, acrylic) in linear light; with HDR off it blends encoded
  values. These pixels legitimately differ between the two references and are
  not SDR codes. Test ROIs should be opaque app content, and a browser in full
  screen avoids them.
- **Applications with their own SDR white.** An application that places SDR
  content at its own white level, not Windows', will be off the grid. Without
  HDR content in the frame it is reproduced by `C/S` (off by its own
  error). If it adjoins HDR content, it can join that HDR region and be tone
  mapped with it.
- **Region edges.** The rectangle is trimmed by one pixel, so an anti-aliased
  edge pixel where HDR content blends into the page is left as captured.
- **Rectangles.** A region is a bounding box. Controls or subtitles drawn over
  HDR video, rounded corners, or two HDR windows close together fall inside
  one box and are tone mapped with it. Codex's review lists these as the
  cases to test with real content.
- **Peak-driven gain.** A single very bright highlight dims its whole region.
  Cropping differently can change the exposure, and video could pump. A
  steadier anchor (a percentile, or a fixed reference-white placement) is
  follow-up work.
- **The panel's peak.** Windows and applications limit HDR content to the
  display's peak luminance before composition, so detail above it is gone from
  the source. No transform can recover it.
- **Little headroom, little room.** At high SDR brightness (`P` ≈ 1.06 on
  this panel) HDR content is barely above SDR white and the curve barely
  acts; there is nothing to separate.
- **WCG hardware** has not been tested; `S = 1` rests on the SDK's
  "display-referred luminance" description.
- **Out of scope for the capture:** ICC calibration, Night light and panel
  processing happen after composition, so they are not in the capture.

## If the gate fails

The gate report's transfer section decides where to look. If the measured
white over `S` is not 1, or piecewise sRGB's worst implied-code error exceeds
about half a code, the model is wrong and the fix belongs in steps 1–3. If the
model holds but the comparison fails, the application drew different codes in
the two modes: look at the heatmap's worst blocks and `probe` them. Record any
finding here before changing constants.
