//! capture-spike: Framecut's Milestone 0 executable. Run with no arguments
//! for usage.

#[cfg(not(windows))]
compile_error!("capture-spike targets Windows 11 only");

use anyhow::{Context, Result, bail, ensure};
use capture_spike::{
    analysis::{self, Roi},
    color::{self, Highlights},
    display,
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
                        [--highlights shoulder|clip]
      Capture one monitor into DIR/NAME-TIMESTAMP/ (default ./captures).
      M is a list index or a device name such as DISPLAY1; default: the
      monitor under the pointer when the capture starts.

  capture-spike convert SOURCE.fp16 OUT.png [--highlights shoulder|clip]
      Re-run the transform on a saved FP16 frame.

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
            "HDR content present: frame peak {:.2}x SDR white; highlights compressed near it",
            shot.frame_peak
        );
    } else {
        println!("No pixel exceeds SDR white: SDR content is reproduced as captured");
    }
    println!("Saved {}", shot.dir.display());
}

fn convert(mut args: Args) -> Result<()> {
    let highlights = highlights(&mut args)?;
    let source = PathBuf::from(args.positional("SOURCE.fp16")?);
    let out = PathBuf::from(args.positional("OUT.png")?);
    args.finish()?;
    let raw = RawFrame::read(&source)?;
    let gpu = Gpu::hardware(None)?;
    let texture = gpu.upload_rgba16f(raw.width, raw.height, &raw.data)?;
    let sdr = SdrConverter::new(&gpu)?.convert(&gpu, &texture, raw.white_scale, highlights)?;
    png_io::write_srgb(&out, sdr.width, sdr.height, &sdr.rgba)?;
    println!(
        "{}x{}, S = {}, frame peak {:.2}x SDR white, highlights {} -> {}",
        sdr.width,
        sdr.height,
        raw.white_scale,
        sdr.frame_peak,
        highlights.name(),
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
            let (mask, near) = analysis::near_hdr_mask(&raw);
            if near > 0 {
                println!("Leaving out {near} px near HDR content");
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
    print!("{}", analysis::transfer(&rgba, &raw, roi)?.markdown());
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

fn highlights(args: &mut Args) -> Result<Highlights> {
    args.option("--highlights")?
        .map(|h| Highlights::parse(&h))
        .transpose()
        .map(|h| h.unwrap_or(Highlights::Shoulder))
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
