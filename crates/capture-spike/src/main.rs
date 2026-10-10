//! capture-spike: Shuttercrab's Milestone 0 executable. Run with no arguments
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
    System::{
        Com::{COINIT_MULTITHREADED, CoInitializeEx},
        WinRT::{RO_INIT_MULTITHREADED, RoInitialize},
    },
    UI::HiDpi::{DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, SetProcessDpiAwarenessContext},
};

const USAGE: &str = "\
capture-spike: prove Shuttercrab's HDR/WCG -> SDR capture path (Milestone 0)

USAGE
  capture-spike list
      Enumerate monitors: bounds, DPI, Advanced Color and HDR state, SDR white level.

  capture-spike capture [--monitor M] [--delay SECONDS] [--label NAME] [--out DIR]
                        [--highlights tonemap|clip]
      Capture one monitor into DIR/NAME-TIMESTAMP/ (default ./captures).
      M is a list index or a device name such as DISPLAY1; default: the
      monitor under the pointer when the capture starts.

  capture-spike window HWND OUT.png
      Capture one window, and nothing else on screen, through the app's window
      capture (converted to SDR as the app would). HWND is decimal or 0x hex.

  capture-spike convert SOURCE.fp16 OUT.png [--highlights tonemap|clip]
                        [--anchor peak|pNN|BRIGHTNESS]
      Re-run the transform on a saved FP16 frame. --anchor picks what sets an
      HDR region's exposure: its peak, a percentile of its tile peaks
      (p90, Shuttercrab's and the default), or a fixed brightness over SDR white (4.2).

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

  capture-spike hdr MONITOR on|off
      Turn HDR on or off for one monitor (DISPLAY2, or an index from list),
      as the Settings app does, and print its state afterwards.

  capture-spike scale MONITOR [PERCENT]
      Print one monitor's display scale and the scales it allows, or set it.

  capture-spike refresh MONITOR [HZ]
      Print one monitor's refresh rate and the rates its resolution allows,
      or set it until it is changed back or Windows restarts.

  capture-spike record [--monitor M] [--region X,Y,W,H] [--seconds N] [--fps 30|60]
                       [--pause AT,FOR] [--cursor on] [--sound on] [--out FILE.mp4]
                       [--repeat N] [--window HWND] [--sound-of PID]
      Record H.264 MP4 through the Milestone 3 pipeline (default: 10 s at
      30 fps, the whole monitor, ./captures/recording-TIMESTAMP.mp4). With
      --pause, pause AT seconds in for FOR seconds, then carry on; the
      paused time is left out of the video. With --repeat, record N takes in
      a row (FILE-1.mp4, FILE-2.mp4…) and print this process's private and
      graphics memory before and after each, to find leaks. With --sound on,
      also record what the speakers play, as an AAC track. With --window,
      record that window (its handle; 0x... for hex) wherever it goes. With
      --sound-of, record only that process's share of the speakers' sound.

  capture-spike play FILE.mp4 [--size WxH] [--seconds N] [--sound on] [--frame OUT.png]
                     [--repeat N]
      Play a recording through the app's player for N seconds (default 10),
      muted unless --sound on, taking each new picture at WxH (default: fit
      in 1600x900) at the display's refresh, and print the cost: time per
      picture, CPU and memory, and a seek. --frame saves the first picture.
      --repeat plays it N times over, to find leaks.

  capture-spike ocr IMAGE.png [--language TAG] [--upscale N] [--boxes OUT.png]
      Read the text in an image with Windows' OCR, as the app's text actions
      do, and print the OCR languages installed, the time it took, and each
      line with its words' boxes. --upscale reads it N times larger (smoothly) first;
      --boxes saves the image with each word outlined.

  capture-spike shots [--monitor M] [--repeat N]
      Freeze the monitor and take a full screenshot through the app's capture
      service N times (default 5), printing memory around each. Saves nothing.

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
        "Shuttercrab requires Windows 11 (this is build {build})"
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
        Some("window") => window(args),
        Some("convert") => convert(args),
        Some("compare") => compare(args),
        Some("transfer") => transfer(args),
        Some("fit") => fit(args),
        Some("probe") => probe(args),
        Some("gate") => gate(args),
        Some("gate-report") => gate_report(args),
        Some("hdr-fixture") => hdr_fixture(args),
        Some("hdr") => hdr(args),
        Some("scale") => scale(args),
        Some("refresh") => refresh(args),
        Some("record") => record(args),
        Some("shots") => shots(args),
        Some("play") => play(args),
        Some("ocr") => ocr(args),
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

fn hdr(mut args: Args) -> Result<()> {
    let spec = args.positional("MONITOR")?;
    let on = match args.positional("on|off")?.as_str() {
        "on" => true,
        "off" => false,
        other => bail!("expected on or off, not {other:?}"),
    };
    args.finish()?;
    let monitors = display::enumerate()?;
    let device_name = display::select(&monitors, Some(&spec))?.device_name.clone();
    display::set_hdr(&device_name, on)?;
    let monitors = display::enumerate()?;
    println!("{}", display::find(&monitors, &device_name)?.describe());
    Ok(())
}

/// The device name of the monitor `spec` names.
fn device_name(spec: &str) -> Result<String> {
    let monitors = display::enumerate()?;
    Ok(display::select(&monitors, Some(spec))?.device_name.clone())
}

fn scale(mut args: Args) -> Result<()> {
    let spec = args.positional("MONITOR")?;
    let percent = args.positional("PERCENT").ok();
    args.finish()?;
    let name = device_name(&spec)?;
    if let Some(percent) = percent {
        display::set_dpi_scale(&name, percent.trim_end_matches('%').parse()?)?;
    }
    let (current, allowed) = display::dpi_scale(&name)?;
    println!("{name}: {current}% (allows {allowed:?})");
    Ok(())
}

fn refresh(mut args: Args) -> Result<()> {
    let spec = args.positional("MONITOR")?;
    let hz = args.positional("HZ").ok();
    args.finish()?;
    let name = device_name(&spec)?;
    if let Some(hz) = hz {
        display::set_refresh_rate(&name, hz.trim_end_matches("Hz").trim().parse()?)?;
    }
    let (current, rates) = display::refresh_rate(&name)?;
    println!("{name}: {current} Hz (allows {rates:?})");
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

fn window(mut args: Args) -> Result<()> {
    let hwnd = args.positional("HWND")?;
    let out = PathBuf::from(args.positional("OUT.png")?);
    args.finish()?;
    let hwnd = match hwnd.strip_prefix("0x") {
        Some(hex) => u64::from_str_radix(hex, 16)?,
        None => hwnd.parse()?,
    };
    let hwnd = shuttercrab_capture::WindowId::from_raw(hwnd);
    let capture = shuttercrab_capture::Capture::start()?;
    let shot = futures::executor::block_on(capture.capture_window(hwnd, false))?;
    std::fs::write(&out, shot.png.as_slice())?;
    println!(
        "{}x{} window saved to {}",
        shot.width,
        shot.height,
        out.display()
    );
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

#[expect(
    clippy::too_many_lines,
    reason = "a test command: its options, the recording, then what it measured, in order"
)]
fn record(mut args: Args) -> Result<()> {
    use shuttercrab_capture::{
        MonitorId, PhysicalRect,
        record::{RecordOptions, Recorder},
    };
    let monitor = args.option("--monitor")?;
    let region = args
        .option("--region")?
        .map(|r| Roi::parse(&r))
        .transpose()?
        .map(|r| PhysicalRect::new(r.x as i32, r.y as i32, r.width, r.height));
    let seconds: f64 = args
        .option("--seconds")?
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(10.0);
    let fps: u32 = args
        .option("--fps")?
        .map(|f| f.parse())
        .transpose()?
        .unwrap_or(30);
    let pause = args
        .option("--pause")?
        .map(|p| -> Result<(f64, f64)> {
            let (at, length) = p
                .split_once(',')
                .context("--pause takes AT,FOR in seconds")?;
            Ok((at.parse()?, length.parse()?))
        })
        .transpose()?;
    let include_cursor = args.option("--cursor")?.as_deref() == Some("on");
    let system_sound = args.option("--sound")?.as_deref() == Some("on");
    // Only this process's share of the speakers' sound.
    let sound_process = args
        .option("--sound-of")?
        .map(|p| p.parse::<u32>())
        .transpose()
        .context("--sound-of takes a process id")?;
    // A window's handle, as a number (0x... for hex): record it instead.
    let window = args
        .option("--window")?
        .map(|w| match w.strip_prefix("0x") {
            Some(hex) => u64::from_str_radix(hex, 16),
            None => w.parse(),
        })
        .transpose()
        .context("--window takes a window handle")?
        .map(shuttercrab_capture::WindowId::from_raw);
    let repeat: u32 = args
        .option("--repeat")?
        .map(|r| r.parse())
        .transpose()?
        .unwrap_or(1);
    let out = args.option("--out")?.map(PathBuf::from).unwrap_or_else(|| {
        PathBuf::from("captures").join(format!("recording-{}.mp4", snapshot::timestamp()))
    });
    args.finish()?;
    if let Some(dir) = out.parent().filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let monitors = display::enumerate()?;
    let target = display::select(&monitors, monitor.as_deref())?;
    println!(
        "Recording {} for {seconds} s at {fps} fps...",
        target.device_name
    );
    if repeat > 1 {
        print_memory("before");
    }
    for take in 1..=repeat {
        let path = if repeat > 1 {
            out.with_file_name(format!(
                "{}-{take}.mp4",
                out.file_stem().unwrap_or_default().to_string_lossy()
            ))
        } else {
            out.clone()
        };
        let recorder = Recorder::start(RecordOptions {
            monitor: MonitorId::from_raw(target.hmonitor.0 as u64),
            region,
            window,
            fps,
            include_cursor,
            system_sound: system_sound || sound_process.is_some(),
            sound_process,
            microphone: false,
            microphone_device: None,
            sound_track: false,
            path,
        })?;
        // Count from when recording is running, not from before its setup.
        let started = std::time::Instant::now();
        let wait_until = |t: f64| {
            let left = t - started.elapsed().as_secs_f64();
            if left > 0.0 {
                std::thread::sleep(Duration::from_secs_f64(left));
            }
        };
        if let Some((at, length)) = pause {
            wait_until(at);
            recorder.pause();
            println!("paused at {at} s");
            wait_until(at + length);
            recorder.resume();
            println!("resumed at {} s", at + length);
            wait_until(seconds + length);
        } else {
            wait_until(seconds);
        }
        if repeat > 1 {
            print_memory(&format!("during take {take}"));
        }
        let summary = recorder.stop()?;
        println!(
            "{}x{}, {} frames ({} dropped: encoder busy, {} skipped: over {fps} fps), {:.3} s long, {:.3} s paused, {} encoder, {} -> {}",
            summary.width,
            summary.height,
            summary.frames,
            summary.dropped_busy,
            summary.skipped_rate,
            summary.duration.as_secs_f64(),
            summary.paused.as_secs_f64(),
            if summary.hardware_encoder {
                "hardware"
            } else {
                "software"
            },
            if summary.sound { "sound" } else { "no sound" },
            summary.path.display()
        );
        if let Some(timing) = &summary.timing {
            println!("  {timing}");
        }
        if repeat > 1 {
            // What the take left behind, once Windows has had a moment.
            std::thread::sleep(Duration::from_secs(2));
            print_memory(&format!("after take {take}"));
        }
    }
    Ok(())
}

/// Print this process's private memory and its use of the graphics
/// adapters' own memory.
fn print_memory(when: &str) {
    println!(
        "  memory {when}: {}",
        shuttercrab_platform::memory::Usage::now()
    );
}

/// Freeze a monitor and take a full screenshot of it through the app's
/// capture service, again and again, printing memory after each: what does
/// a screenshot leave behind? Nothing is saved.
fn shots(mut args: Args) -> Result<()> {
    use shuttercrab_capture::{Capture, MonitorId, PhysicalRect};
    let monitor = args.option("--monitor")?;
    let repeat: u32 = args
        .option("--repeat")?
        .map(|r| r.parse())
        .transpose()?
        .unwrap_or(5);
    args.finish()?;
    let monitors = display::enumerate()?;
    let target = display::select(&monitors, monitor.as_deref())?;
    let id = MonitorId::from_raw(target.hmonitor.0 as u64);
    let capture = Capture::start()?;
    print_memory("before");
    for shot in 1..=repeat {
        let started = std::time::Instant::now();
        let frame = futures::executor::block_on(capture.freeze_monitor(id, false))?;
        println!("  froze in {} ms", started.elapsed().as_millis());
        print_memory(&format!("frozen {shot}"));
        let (width, height) = frame.size();
        let whole = PhysicalRect::new(0, 0, width, height);
        let screenshot = shuttercrab_capture::cut(frame.bgra(), width, height, whole)?;
        print_memory(&format!("holding shot {shot} ({width}x{height})"));
        drop(screenshot);
        drop(frame);
        std::thread::sleep(Duration::from_secs(1));
        print_memory(&format!("after shot {shot}"));
    }
    Ok(())
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

/// `--anchor peak`, `--anchor p90` (Shuttercrab's, the default) (a percentile of the region's
/// tile peaks) or `--anchor 4.2` (a fixed brightness over SDR white, such as
/// the display's peak).
fn anchor(args: &mut Args) -> Result<Anchor> {
    let Some(text) = args.option("--anchor")? else {
        // What Shuttercrab uses.
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

/// Read an image's text with the app's OCR, and print what it found.
fn ocr(mut args: Args) -> Result<()> {
    use shuttercrab_platform::ocr;
    let image = PathBuf::from(args.positional("IMAGE.png")?);
    let language = args.option("--language")?;
    let upscale: u32 = args
        .option("--upscale")?
        .map(|n| n.parse())
        .transpose()?
        .unwrap_or(1);
    let boxes = args.option("--boxes")?.map(PathBuf::from);
    args.finish()?;
    ensure!(upscale >= 1, "--upscale takes 1 or more");

    let installed: Vec<String> = ocr::languages()?
        .into_iter()
        .map(|l| format!("{} ({})", l.tag, l.name))
        .collect();
    println!("OCR languages: {}", installed.join(", "));
    println!("Largest side: {} px", ocr::max_side()?);

    let (width, height, rgba) = png_io::read_rgba8(&image)?;
    let mut bgra = rgba.clone();
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    let started = std::time::Instant::now();
    let text = ocr::read(&bgra, width, height, language.as_deref(), upscale)?;
    let took = started.elapsed();
    let words: usize = text.lines.iter().map(|l| l.words.len()).sum();
    println!(
        "Read {width}x{height} x{upscale} in {} ms as {}: {} lines, {words} words, angle {:?}",
        took.as_millis(),
        text.language,
        text.lines.len(),
        text.angle
    );
    for line in &text.lines {
        println!("  {}", line.text);
        let placed: Vec<String> = line
            .words
            .iter()
            .map(|w| {
                let r = w.rect;
                format!(
                    "{}@{:.0},{:.0},{:.0}x{:.0}",
                    w.text, r.x, r.y, r.width, r.height
                )
            })
            .collect();
        println!("    {}", placed.join(" "));
    }
    if let Some(out) = boxes {
        let mut outlined = rgba;
        for word in text.lines.iter().flat_map(|l| &l.words) {
            outline(&mut outlined, width, height, word.rect);
        }
        png_io::write_srgb(&out, width, height, &outlined)?;
        println!("Wrote {}", out.display());
    }
    Ok(())
}

/// A magenta box around `rect`, a pixel wide.
fn outline(rgba: &mut [u8], width: u32, height: u32, rect: shuttercrab_platform::ocr::Rect) {
    let x0 = (rect.x.max(0.) as u32).min(width - 1);
    let y0 = (rect.y.max(0.) as u32).min(height - 1);
    let x1 = ((rect.x + rect.width) as u32).min(width - 1);
    let y1 = ((rect.y + rect.height) as u32).min(height - 1);
    let mut set = |x: u32, y: u32| {
        let at = ((y * width + x) * 4) as usize;
        rgba[at..at + 4].copy_from_slice(&[255, 0, 255, 255]);
    };
    for x in x0..=x1 {
        set(x, y0);
        set(x, y1);
    }
    for y in y0..=y1 {
        set(x0, y);
        set(x1, y);
    }
}

/// Play a recording through the app's player for a while, pulling each new
/// picture at the display's refresh as the app would, and print what that
/// costs.
fn play(mut args: Args) -> Result<()> {
    let file = PathBuf::from(args.positional("FILE.mp4")?);
    let size = args
        .option("--size")?
        .map(|s| -> Result<(u32, u32)> {
            let (w, h) = s.split_once('x').context("--size takes WxH")?;
            Ok((w.parse()?, h.parse()?))
        })
        .transpose()?;
    let seconds: f64 = args
        .option("--seconds")?
        .map(|s| s.parse())
        .transpose()?
        .unwrap_or(10.0);
    let sound = args.option("--sound")?.as_deref() == Some("on");
    let frame_out = args.option("--frame")?.map(PathBuf::from);
    let repeat: u32 = args
        .option("--repeat")?
        .map(|r| r.parse())
        .transpose()?
        .unwrap_or(1);
    args.finish()?;
    unsafe { CoInitializeEx(None, COINIT_MULTITHREADED).ok()? };

    print_memory("before");
    for take in 1..=repeat {
        if repeat > 1 {
            println!("take {take}");
        }
        play_once(&file, size, seconds, sound, frame_out.as_deref())?;
    }
    Ok(())
}

fn play_once(
    file: &std::path::Path,
    size: Option<(u32, u32)>,
    seconds: f64,
    sound: bool,
    frame_out: Option<&std::path::Path>,
) -> Result<()> {
    use shuttercrab_capture::play::{Player, PlayerEvent};
    use std::{sync::mpsc, time::Instant};
    let (tx, events) = mpsc::channel();
    let opened = Instant::now();
    let mut player = Player::open(file, move |e| {
        let _ = tx.send(e);
    })?;
    print_memory("opened");
    let wait = |want: PlayerEvent| -> Result<()> {
        loop {
            match events.recv_timeout(Duration::from_secs(10))? {
                PlayerEvent::Failed(why) => bail!("cannot play: {why}"),
                e if e == want => return Ok(()),
                e => println!("  {e:?}"),
            }
        }
    };
    wait(PlayerEvent::Loaded)?;
    let (w, h) = player.size().context("the file has no pictures")?;
    let duration = player.duration();
    println!(
        "{w}x{h}, {:.3} s long, loaded in {} ms",
        duration.as_secs_f64(),
        opened.elapsed().as_millis()
    );
    let (fw, fh) = size.unwrap_or_else(|| {
        let scale = (1600.0 / f64::from(w)).min(900.0 / f64::from(h)).min(1.0);
        (
            (f64::from(w) * scale).round() as u32,
            (f64::from(h) * scale).round() as u32,
        )
    });
    player.set_muted(!sound)?;
    wait(PlayerEvent::FirstFrame)?;
    let first = player.frame(fw, fh)?;
    println!(
        "first picture ({fw}x{fh}) after {} ms",
        opened.elapsed().as_millis()
    );
    if let Some(out) = frame_out {
        let mut rgba = first;
        for px in rgba.as_chunks_mut::<4>().0 {
            px.swap(0, 2);
        }
        png_io::write_srgb(out, fw, fh, &rgba)?;
        println!("  saved {}", out.display());
    }
    print_memory("loaded");

    let cpu_before = cpu_time();
    let started = Instant::now();
    player.play()?;
    let (mut took, mut refreshes) = (Vec::new(), 0u32);
    while started.elapsed().as_secs_f64() < seconds && !player.is_ended() {
        player.wait_for_refresh()?;
        refreshes += 1;
        let asked = Instant::now();
        if player.next_frame(fw, fh)?.is_some() {
            took.push(asked.elapsed());
        }
        while let Ok(e) = events.try_recv() {
            match e {
                PlayerEvent::Failed(why) => bail!("cannot play: {why}"),
                e => println!("  {e:?} at {:.2} s", started.elapsed().as_secs_f64()),
            }
        }
    }
    let wall = started.elapsed();
    let cpu = cpu_time() - cpu_before;
    print_memory("playing");
    took.sort();
    let ms = |i: usize| took.get(i).map_or(0.0, |d| d.as_secs_f64() * 1000.0);
    println!(
        "{} pictures in {:.2} s ({:.1} a second) over {refreshes} refreshes; \
         getting one: median {:.2} ms, 95% {:.2} ms, slowest {:.2} ms; \
         CPU {:.0}% of one core",
        took.len(),
        wall.as_secs_f64(),
        took.len() as f64 / wall.as_secs_f64(),
        ms(took.len() / 2),
        ms(took.len() * 95 / 100),
        ms(took.len().saturating_sub(1)),
        cpu.as_secs_f64() / wall.as_secs_f64() * 100.0
    );

    player.pause()?;
    seek_and_time(&mut player, &events, duration / 2, (fw, fh))?;
    drop(player);
    // What playing left behind, once Windows has had a moment.
    std::thread::sleep(Duration::from_secs(2));
    print_memory("after");
    Ok(())
}

/// The CPU time this process has used, all threads.
fn cpu_time() -> Duration {
    use windows::Win32::{
        Foundation::FILETIME,
        System::Threading::{GetCurrentProcess, GetProcessTimes},
    };
    let (mut created, mut exited, mut kernel, mut user) = Default::default();
    let ticks = |t: FILETIME| u64::from(t.dwHighDateTime) << 32 | u64::from(t.dwLowDateTime);
    unsafe {
        if GetProcessTimes(
            GetCurrentProcess(),
            &mut created,
            &mut exited,
            &mut kernel,
            &mut user,
        )
        .is_err()
        {
            return Duration::ZERO;
        }
    }
    Duration::from_nanos((ticks(kernel) + ticks(user)) * 100)
}

/// Seek a paused player to `at`, asking for pictures until the seek has
/// finished, and print how long the new picture and the seek took.
fn seek_and_time(
    player: &mut shuttercrab_capture::play::Player,
    events: &std::sync::mpsc::Receiver<shuttercrab_capture::play::PlayerEvent>,
    at: Duration,
    (fw, fh): (u32, u32),
) -> Result<()> {
    use shuttercrab_capture::play::PlayerEvent;
    let seeking = std::time::Instant::now();
    player.seek(at)?;
    // In frame server mode the engine finishes a seek only once pictures
    // are asked for.
    let (mut seeked, mut shown) = (None, None);
    while seeked.is_none() || shown.is_none() {
        ensure!(
            seeking.elapsed() < Duration::from_secs(10),
            "the seek did not finish"
        );
        player.wait_for_refresh()?;
        if player.next_frame(fw, fh)?.is_some() && shown.is_none() {
            shown = Some(seeking.elapsed());
        }
        while let Ok(e) = events.try_recv() {
            match e {
                PlayerEvent::Seeked => seeked = Some(seeking.elapsed()),
                PlayerEvent::Failed(why) => bail!("cannot seek: {why}"),
                e => println!("  {e:?}"),
            }
        }
    }
    let ms = |d: Option<Duration>| d.unwrap_or_default().as_millis();
    println!(
        "seek to {:.1} s: picture after {} ms, finished after {} ms",
        at.as_secs_f64(),
        ms(shown),
        ms(seeked)
    );
    Ok(())
}
