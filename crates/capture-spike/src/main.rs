//! capture-spike: Framecut's Milestone 0 executable. Run with no arguments
//! for usage.

#[cfg(not(windows))]
compile_error!("capture-spike targets Windows 11 only");

use anyhow::{Context, Result, bail, ensure};
use capture_spike::{
    analysis::{self, Roi},
    color::{self, Anchor, Highlights},
    display, fixture, gate,
    gpu::{Gpu, SdrConverter},
    png_io,
    raw::RawFrame,
    snapshot,
};
use std::{path::PathBuf, time::Duration};
use windows::Win32::{
    System::WinRT::{RO_INIT_MULTITHREADED, RoInitialize},
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
};

const USAGE: &str = "\
capture-spike: prove Framecut's HDR/WCG -> SDR capture path (Milestone 0)

USAGE
  capture-spike list
      Enumerate monitors: bounds, DPI, Advanced Color and HDR state, SDR white level.

  capture-spike capture [--monitor M] [--delay SECONDS] [--label NAME] [--out DIR]
                        [--highlights tonemap|clip]
      Capture one monitor into DIR/NAME-TIMESTAMP/ (default ./captures).
      M is a list index or a device name such as DISPLAY1; default: the
      monitor under the pointer when the capture starts.

  capture-spike convert SOURCE.fp16 OUT.png [--highlights tonemap|clip]
                        [--anchor peak|pNN|BRIGHTNESS]
      Re-run the transform on a saved FP16 frame. --anchor picks what sets an
      HDR region's exposure: its peak, a percentile of its tile peaks
      (p90, Framecut's and the default), or a fixed brightness over SDR white (4.2).

  capture-spike compare REFERENCE.png TEST.png [--roi X,Y,W,H] [--source TEST.fp16]
                        [--heatmap OUT.png]
      Pixel-for-pixel CIEDE2000 and code-value comparison of two captures of
      the same screen. With --source (the FP16 frame behind TEST), pixels near
      HDR content are left out. Exits with status 2 on FAIL.

  capture-spike transfer REFERENCE.png SOURCE.fp16 [--roi X,Y,W,H]
      For each grey code in an HDR-off reference, the value the HDR-on FP16
      frame holds at the same pixels: the curve Windows actually used.

  capture-spike fit SOURCE.fp16 [--roi X,Y,W,H] [--map OUT.png]
      Without a reference: how well piecewise sRGB and gamma 2.2 explain the
      FP16 values as 8-bit codes at the recorded SDR white level.

  capture-spike probe SOURCE.fp16 X,Y,W,H
      Mean, min and max FP16 values over a region, relative to SDR white.

  capture-spike gate [--monitor M] [--scenes fixture,mixed] [--states hdr-off,...]
                     [--delay SECONDS] [--out DIR]
      The Milestone 0 gate: a guided session that captures each scene with HDR
      off and at three SDR content brightness settings, then writes summary.md.
      Default delay 5 s, default directory captures/gate-TIMESTAMP. With --out
      set to an existing session and --states, retakes only those states.

  capture-spike gate-report DIR [--scenes fixture,mixed]
      Re-run the gate analysis on a session (by default, every scene in it).

  capture-spike hdr-fixture OUT.png
      Write the HDR test image (BT.2020 PQ PNG) used by the mixed scene.

Run `capture-spike help` for this text.";

fn main() {
    if let Err(e) = run() {
        eprintln!("capture-spike: {e:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    let build = snapshot::windows_build();
    ensure!(
        build >= 22000,
        "Framecut requires Windows 11 (this is build {build})"
    );
    unsafe {
        // Physical pixels everywhere; monitor bounds and DPI depend on it.
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)?;
        RoInitialize(RO_INIT_MULTITHREADED)?;
    }
    let mut args = Args::new(std::env::args().skip(1).collect());
    match args.command().as_deref() {
        Some("list") => list(args),
        Some("capture") => capture(args),
        Some("convert") => convert(args),
        Some("compare") => compare(args),
        Some("transfer") => transfer(args),
        Some("fit") => fit(args),
        Some("probe") => probe(args),
        Some("gate") => gate(args),
        Some("gate-report") => gate_report(args),
        Some("hdr-fixture") => hdr_fixture(args),
        Some("help") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => bail!("unknown command {other:?}\n\n{USAGE}"),
    }
}

fn list(args: Args) -> Result<()> {
    args.finish()?;
    println!("Windows build {}", snapshot::windows_build());
    for (i, monitor) in display::enumerate()?.iter().enumerate() {
        println!("[{i}] {}", monitor.describe());
    }
    Ok(())
}

fn capture(mut args: Args) -> Result<()> {
    let monitor = args.option("--monitor")?;
    let delay: f64 = args
        .option("--delay")?
        .map(|d| d.parse())
        .transpose()?
        .unwrap_or(0.0);
    let label = args.option("--label")?.unwrap_or_else(|| "capture".into());
    let out = PathBuf::from(args.option("--out")?.unwrap_or_else(|| "captures".into()));
    let highlights = highlights(&mut args)?;
    args.finish()?;
    ensure!(
        !label.is_empty()
            && label
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "--label may only contain letters, digits, '-' and '_'"
    );
    if delay > 0.0 {
        println!("Capturing in {delay} s...");
        std::thread::sleep(Duration::from_secs_f64(delay));
    }
    let dir = out.join(format!("{label}-{}", snapshot::timestamp()));
    let shot = snapshot::take(monitor.as_deref(), &dir, highlights)?;
    print_snapshot(&shot);
    Ok(())
}

pub fn print_snapshot(shot: &snapshot::Snapshot) {
    let white = match shot.sdr_white_level {
        Some(raw) => format!("{raw} ({:.0} nits)", color::sdr_white_nits(raw)),
        None => "not reported".into(),
    };
    println!(
        "{}: {}x{}, {}, SDR white level {white}, S = {}",
        shot.device_name,
        shot.width,
        shot.height,
        shot.color_mode.name(),
        shot.white_scale
    );
    if shot.frame_peak > 1.0 + color::EXTENDED_EPSILON {
        println!(
            "HDR content present: frame peak {:.2}x SDR white; HDR regions tone mapped",
            shot.frame_peak
        );
    } else {
        println!("No pixel exceeds SDR white: SDR content is reproduced as captured");
    }
    println!("Saved {}", shot.dir.display());
}

fn convert(mut args: Args) -> Result<()> {
    let highlights = highlights(&mut args)?;
    let anchor = anchor(&mut args)?;
    let source = PathBuf::from(args.positional("SOURCE.fp16")?);
    let out = PathBuf::from(args.positional("OUT.png")?);
    args.finish()?;
    let raw = RawFrame::read(&source)?;
    let gpu = Gpu::hardware(None)?;
    let texture = gpu.upload_rgba16f(raw.width, raw.height, &raw.data)?;
    let sdr = SdrConverter::new(&gpu)?.convert_anchored(
        &gpu,
        &texture,
        raw.white_scale,
        highlights,
        anchor,
    )?;
    png_io::write_srgb(&out, sdr.width, sdr.height, &sdr.rgba)?;
    println!(
        "{}x{}, S = {}, frame peak {:.2}x SDR white, highlights {}, anchor {anchor:?}, regions {:?} -> {}",
        sdr.width,
        sdr.height,
        raw.white_scale,
        sdr.frame_peak,
        highlights.name(),
        sdr.regions
            .iter()
            .map(|r| format!("{:.2}", r.peak))
            .collect::<Vec<_>>(),
        out.display()
    );
    Ok(())
}

fn compare(mut args: Args) -> Result<()> {
    let roi = args.option("--roi")?;
    let source = args.option("--source")?.map(PathBuf::from);
    let heatmap = args.option("--heatmap")?.map(PathBuf::from);
    let reference = PathBuf::from(args.positional("REFERENCE.png")?);
    let test = PathBuf::from(args.positional("TEST.png")?);
    args.finish()?;
    let (width, height, a) = png_io::read_rgba8(&reference)?;
    let (tw, th, b) = png_io::read_rgba8(&test)?;
    ensure!(
        (width, height) == (tw, th),
        "the images are {width}x{height} and {tw}x{th}"
    );
    let roi = roi
        .map(|r| Roi::parse(&r))
        .transpose()?
        .unwrap_or(Roi::full(width, height));
    let mask = match &source {
        Some(path) => {
            let raw = RawFrame::read(path)?;
            ensure!(
                (raw.width, raw.height) == (width, height),
                "the FP16 source is a different size"
            );
            let region = analysis::hdr_region(&raw);
            let (mask, near) = (region.mask, region.pixels);
            if near > 0 {
                println!("Leaving out {near} px of HDR content");
            }
            Some(mask)
        }
        None => None,
    };
    let result = analysis::compare(&a, &b, width, height, roi, mask.as_deref())?;
    println!("{}", result.summary());
    for block in &result.worst_blocks {
        println!(
            "  64x64 block at ({}, {}): mean ΔE00 {:.3}",
            block.x, block.y, block.mean_de
        );
    }
    if let Some(path) = heatmap {
        png_io::write_srgb(
            &path,
            width,
            height,
            &analysis::heatmap(&a, &result, mask.as_deref()),
        )?;
        println!("Heatmap: {}", path.display());
    }
    if !result.passes() {
        std::process::exit(2);
    }
    Ok(())
}

fn transfer(mut args: Args) -> Result<()> {
    let roi = args.option("--roi")?;
    let reference = PathBuf::from(args.positional("REFERENCE.png")?);
    let source = PathBuf::from(args.positional("SOURCE.fp16")?);
    args.finish()?;
    let (width, height, rgba) = png_io::read_rgba8(&reference)?;
    let raw = RawFrame::read(&source)?;
    ensure!(
        (raw.width, raw.height) == (width, height),
        "the FP16 source is a different size"
    );
    let roi = roi
        .map(|r| Roi::parse(&r))
        .transpose()?
        .unwrap_or(Roi::full(width, height));
    // HDR content has no SDR code to measure; leave its region out.
    let region = analysis::hdr_region(&raw);
    let exclude = (region.pixels > 0).then_some(&region.mask[..]);
    print!(
        "{}",
        analysis::transfer(&rgba, &raw, roi, exclude)?.markdown()
    );
    Ok(())
}

fn fit(mut args: Args) -> Result<()> {
    let roi = args.option("--roi")?;
    let map = args.option("--map")?.map(PathBuf::from);
    let source = PathBuf::from(args.positional("SOURCE.fp16")?);
    args.finish()?;
    let raw = RawFrame::read(&source)?;
    let roi = roi
        .map(|r| Roi::parse(&r))
        .transpose()?
        .unwrap_or(Roi::full(raw.width, raw.height));
    println!("S = {} ({})", raw.white_scale, raw.color_mode.name());
    print!(
        "{}",
        analysis::code_fit_markdown(&analysis::code_fit(&raw, roi)?)
    );
    if let Some(path) = map {
        let rgba = analysis::code_fit_map(&raw, analysis::Curve::Srgb);
        png_io::write_srgb(&path, raw.width, raw.height, &rgba)?;
        println!("Off-code map (piecewise sRGB): {}", path.display());
    }
    Ok(())
}

fn probe(mut args: Args) -> Result<()> {
    let source = PathBuf::from(args.positional("SOURCE.fp16")?);
    let roi = Roi::parse(&args.positional("X,Y,W,H")?)?;
    args.finish()?;
    let raw = RawFrame::read(&source)?;
    roi.check(raw.width, raw.height)?;
    let s = raw.white_scale;
    let (mut sum, mut lo, mut hi) = ([0.0f64; 3], [f32::MAX; 3], [f32::MIN; 3]);
    for y in roi.y..roi.y + roi.height {
        for x in roi.x..roi.x + roi.width {
            let p = raw.pixel(x, y);
            for c in 0..3 {
                sum[c] += p[c] as f64;
                lo[c] = lo[c].min(p[c]);
                hi[c] = hi[c].max(p[c]);
            }
        }
    }
    let n = (roi.width * roi.height) as f64;
    let curve = analysis::Curve::Srgb;
    println!("S = {s}; values over S, and the code piecewise sRGB implies:");
    for (c, name) in ["R", "G", "B"].iter().enumerate() {
        let mean = sum[c] / n;
        println!(
            "  {name}: mean {:.6} (code {:.3}), min {:.6}, max {:.6}",
            mean / s as f64,
            curve.encode(mean / s as f64),
            lo[c] / s,
            hi[c] / s
        );
    }
    Ok(())
}

/// `--anchor peak`, `--anchor p90` (Framecut's, the default) (a percentile of the region's
/// tile peaks) or `--anchor 4.2` (a fixed brightness over SDR white, such as
/// the display's peak).
fn anchor(args: &mut Args) -> Result<Anchor> {
    let Some(text) = args.option("--anchor")? else {
        // What Framecut uses.
        return Ok(color::SCREENSHOT_ANCHOR);
    };
    if text == "peak" {
        return Ok(Anchor::RegionPeak);
    }
    if let Some(q) = text.strip_prefix('p') {
        let q: f32 = q.parse().context("--anchor pNN: NN is a percentage")?;
        return Ok(Anchor::Percentile(q / 100.0));
    }
    let fixed: f32 = text
        .parse()
        .context("--anchor takes peak, pNN or a brightness over SDR white")?;
    Ok(Anchor::Fixed(fixed))
}

fn highlights(args: &mut Args) -> Result<Highlights> {
    args.option("--highlights")?
        .map(|h| Highlights::parse(&h))
        .transpose()
        .map(|h| h.unwrap_or(Highlights::Tonemap))
}

/// Just enough argument parsing for a spike: a command, positionals, and
/// `--name value` options.
struct Args(Vec<String>);

impl Args {
    fn new(args: Vec<String>) -> Self {
        Self(args)
    }

    fn command(&mut self) -> Option<String> {
        (!self.0.is_empty()).then(|| self.0.remove(0))
    }

    fn option(&mut self, name: &str) -> Result<Option<String>> {
        let Some(i) = self.0.iter().position(|a| a == name) else {
            return Ok(None);
        };
        ensure!(i + 1 < self.0.len(), "{name} needs a value");
        self.0.remove(i);
        Ok(Some(self.0.remove(i)))
    }

    fn positional(&mut self, what: &str) -> Result<String> {
        let i = self
            .0
            .iter()
            .position(|a| !a.starts_with("--"))
            .with_context(|| format!("missing {what}"))?;
        Ok(self.0.remove(i))
    }

    fn finish(self) -> Result<()> {
        ensure!(
            self.0.is_empty(),
            "unexpected arguments: {}",
            self.0.join(" ")
        );
        Ok(())
    }
}

fn scenes(args: &mut Args) -> Result<Option<Vec<String>>> {
    let Some(text) = args.option("--scenes")? else {
        return Ok(None);
    };
    let scenes: Vec<String> = text
        .split(',')
        .map(|s| s.trim().to_owned())
        .filter(|s| !s.is_empty())
        .collect();
    ensure!(
        scenes.iter().all(|s| s
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')),
        "scene names may only contain letters, digits, '-' and '_'"
    );
    ensure!(!scenes.is_empty(), "--scenes needs at least one scene");
    Ok(Some(scenes))
}

fn gate(mut args: Args) -> Result<()> {
    let monitor = args.option("--monitor")?;
    let scenes = scenes(&mut args)?.unwrap_or_else(|| vec!["fixture".into(), "mixed".into()]);
    let states = args.option("--states")?;
    let states = gate::parse_states(states.as_deref())?;
    let delay: f64 = args
        .option("--delay")?
        .map(|d| d.parse())
        .transpose()?
        .unwrap_or(5.0);
    let dir = args
        .option("--out")?
        .map(PathBuf::from)
        .unwrap_or_else(gate::default_dir);
    args.finish()?;
    gate::run(monitor.as_deref(), &scenes, &states, &dir, delay)
}

fn gate_report(mut args: Args) -> Result<()> {
    let scenes = scenes(&mut args)?;
    let dir = PathBuf::from(args.positional("DIR")?);
    args.finish()?;
    println!("{}", gate::report(&dir, scenes.as_deref())?);
    Ok(())
}

fn hdr_fixture(mut args: Args) -> Result<()> {
    let out = PathBuf::from(args.positional("OUT.png")?);
    args.finish()?;
    fixture::write_hdr_test_image(&out)?;
    println!("Wrote {}", out.display());
    Ok(())
}
