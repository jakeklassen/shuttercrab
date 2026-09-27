//! The Milestone 0 gate: a guided HDR-off / HDR-on capture session and the
//! report that decides it.
//!
//! Captures land in `DIR/<state>/<scene>/` (see `snapshot`), so `report`
//! can be re-run on a finished session without capturing again.

use crate::{
    analysis::{self, Comparison, Roi},
    color::{self, ColorMode, Highlights},
    display,
    gpu::{Gpu, SdrConverter},
    png_io,
    raw::RawFrame,
    snapshot,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::BTreeSet,
    fmt::Write as _,
    io::{BufRead, Write as _},
    path::{Path, PathBuf},
    time::Duration,
};

/// The display states of the gate, in capture order. The first is the
/// reference; the SDR content brightness values span the slider.
pub const STATES: [(&str, Option<u32>); 4] = [
    ("hdr-off", None),
    ("hdr-low", Some(10)),
    ("hdr-mid", Some(50)),
    ("hdr-high", Some(100)),
];
const REFERENCE: &str = "hdr-off";

fn scene_hint(scene: &str) -> String {
    match scene {
        "fixture" => {
            "fixtures/sdr-reference.html in Edge or Chrome, full screen (F11), zoom 100%".into()
        }
        "mixed" => {
            "fixtures/sdr-reference.html?hdr (the same page with the HDR test image), full screen"
                .into()
        }
        other => format!("{other}, arranged exactly as in the other states"),
    }
}

fn prompt(text: &str) -> Result<String> {
    print!("{text}");
    std::io::stdout().flush()?;
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    let line = line.trim().to_owned();
    if line.eq_ignore_ascii_case("q") {
        bail!("stopped at your request; captures so far are kept");
    }
    Ok(line)
}

/// Run the guided capture session, then the report.
pub fn run(monitor: Option<&str>, scenes: &[String], dir: &Path, delay: f64) -> Result<()> {
    let monitors = display::enumerate()?;
    let target = match monitor {
        Some(spec) => display::select(&monitors, Some(spec))?,
        None if monitors.len() == 1 => &monitors[0],
        None => bail!("several monitors are attached; pass --monitor (see `capture-spike list`)"),
    };
    let device = target.device_name.clone();
    std::fs::create_dir_all(dir)?;
    println!(
        "Framecut Milestone 0 gate on {} ({}), saving to {}\n\
         Type q and Enter at any prompt to stop.\n\n\
         Before starting: Night light off, same resolution and scaling throughout,\n\
         browser zoom 100%, pointer parked away from the page. Captures happen\n\
         {delay} s after you press Enter, so you can switch to the page.\n",
        device,
        target.friendly_name,
        dir.display()
    );

    let mut white_levels: Vec<(String, u32)> = Vec::new();
    for (state, slider) in STATES {
        let instruction = match slider {
            None => format!(
                "Turn HDR OFF for {device}: Settings > System > Display > HDR (Use HDR: Off)."
            ),
            Some(level) => format!(
                "Turn HDR ON for {device} and set Settings > System > Display > HDR > \
                 SDR content brightness to {level}."
            ),
        };
        println!("== {state} ==\n{instruction}");
        loop {
            prompt("Press Enter when it is set... ")?;
            let monitors = display::enumerate()?;
            let m = display::find(&monitors, &device)?;
            let hdr = m.color_mode == ColorMode::Hdr;
            if hdr != slider.is_some() {
                println!(
                    "{device} is in {} mode; that is not this step's setting yet.",
                    m.color_mode.name()
                );
                continue;
            }
            if let Some(level) = m.sdr_white_level.filter(|_| hdr) {
                if let Some((other, _)) = white_levels.iter().find(|(_, l)| *l == level) {
                    println!(
                        "SDR white level is {level}, the same as in {other}. Did the slider move? \
                         (Enter to re-check, or type same to continue anyway)"
                    );
                    if prompt("> ")? != "same" {
                        continue;
                    }
                }
                white_levels.push((state.to_owned(), level));
                println!(
                    "SDR white level {level} ({:.0} nits).",
                    color::sdr_white_nits(level)
                );
            }
            break;
        }
        for scene in scenes {
            loop {
                prompt(&format!(
                    "Show {}.\nPress Enter, then switch to it... ",
                    scene_hint(scene)
                ))?;
                std::thread::sleep(Duration::from_secs_f64(delay));
                let out = dir.join(state).join(scene);
                if out.exists() {
                    std::fs::remove_dir_all(&out)?;
                }
                match snapshot::take(Some(&device), &out, Highlights::Shoulder) {
                    Ok(shot) => {
                        println!(
                            "\x07Captured {state}/{scene}: {}, S = {}, frame peak {:.2}\n",
                            shot.color_mode.name(),
                            shot.white_scale,
                            shot.frame_peak
                        );
                        break;
                    }
                    Err(e) => println!("\x07Capture failed: {e:#}\nTrying again."),
                }
            }
        }
    }
    let summary = report(dir, scenes)?;
    println!("{summary}");
    println!("Written to {}", dir.join("summary.md").display());
    Ok(())
}

struct Capture {
    rgba: Vec<u8>,
    windows_rgba: Vec<u8>,
    raw: RawFrame,
    width: u32,
    height: u32,
    report: serde_json::Value,
}

fn load(dir: &Path) -> Result<Capture> {
    let (width, height, rgba) = png_io::read_rgba8(&dir.join("capture.png"))?;
    let (_, _, windows_rgba) = png_io::read_rgba8(&dir.join("windows-8bit.png"))?;
    let raw = RawFrame::read(&dir.join("source.fp16"))?;
    let report = serde_json::from_str(&std::fs::read_to_string(dir.join("report.json"))?)?;
    Ok(Capture {
        rgba,
        windows_rgba,
        raw,
        width,
        height,
        report,
    })
}

fn or_masks(a: &[bool], b: &[bool]) -> Vec<bool> {
    a.iter().zip(b).map(|(x, y)| *x || *y).collect()
}

/// Analyse a finished gate directory and write `summary.md`.
pub fn report(dir: &Path, scenes: &[String]) -> Result<String> {
    let diffs = dir.join("diffs");
    std::fs::create_dir_all(&diffs)?;
    let mut md = String::new();
    let mut gate_results: Vec<bool> = Vec::new();
    let mut invariance_results: Vec<bool> = Vec::new();

    writeln!(md, "# Framecut Milestone 0 gate\n")?;
    writeln!(
        md,
        "Session `{}`, transform `{}`, capture-spike {}.\n",
        dir.file_name().map_or("?".into(), |n| n.to_string_lossy()),
        color::TRANSFORM_VERSION,
        env!("CARGO_PKG_VERSION")
    )?;

    // Load everything up front; a missing capture is reported, not fatal.
    let mut captures = std::collections::BTreeMap::new();
    for (state, _) in STATES {
        for scene in scenes {
            let path = dir.join(state).join(scene);
            match load(&path) {
                Ok(c) => {
                    captures.insert((state, scene.as_str()), c);
                }
                Err(e) => writeln!(md, "- missing {state}/{scene}: {e:#}")?,
            }
        }
    }
    let first = captures
        .values()
        .next()
        .context("no captures in this directory")?;
    let m = &first.report["monitor"];
    writeln!(
        md,
        "Windows build {}, GPU {}, monitor {} {} ({}x{}, {} DPI).\n",
        first.report["windows_build"],
        first.report["gpu"],
        m["device_name"].as_str().unwrap_or("?"),
        m["friendly_name"].as_str().unwrap_or("?"),
        m["bounds"]["width"],
        m["bounds"]["height"],
        m["dpi"]
    )?;

    writeln!(md, "## Captures\n")?;
    writeln!(
        md,
        "| state | scene | mode | SDR white level | S | frame peak | product path |"
    )?;
    writeln!(md, "|---|---|---|---:|---:|---:|---|")?;
    for (state, scene, c) in in_order(&captures, scenes) {
        let mon = &c.report["monitor"];
        writeln!(
            md,
            "| {state} | {scene} | {} | {} | {} | {:.3} | {} |",
            mon["color_mode"].as_str().unwrap_or("?"),
            mon["sdr_white_level_raw"],
            c.raw.white_scale,
            c.report["frame"]["peak_relative_to_sdr_white"]
                .as_f64()
                .unwrap_or(f64::NAN),
            c.report["product_path"].as_str().unwrap_or("?"),
        )?;
        if let Some(fp) = c.report["fp16_path_vs_windows_8bit"].as_object()
            && c.raw.color_mode == ColorMode::Sdr
        {
            writeln!(
                md,
                "|  | ↳ FP16 path vs 8-bit capture on SDR: max code diff {}, channels >1 code {} | | | | | |",
                fp["max_code_difference"], fp["channels_differing_by_more_than_1"]
            )?;
        }
    }

    let masks: std::collections::BTreeMap<_, _> = captures
        .iter()
        .map(|(key, c)| (*key, analysis::shoulder_mask(&c.raw)))
        .collect();

    let compare_md = |md: &mut String,
                      label: &str,
                      a: &Capture,
                      b: &Capture,
                      b_rgba: &[u8],
                      mask: Option<&[bool]>,
                      heatmap_name: &str|
     -> Result<Comparison> {
        ensure!(
            (a.width, a.height) == (b.width, b.height),
            "{label}: captures differ in size"
        );
        let result = analysis::compare(
            &a.rgba,
            b_rgba,
            a.width,
            a.height,
            Roi::full(a.width, a.height),
            mask,
        )?;
        writeln!(md, "- **{label}**: {}", result.summary())?;
        for block in result.worst_blocks.iter().take(3) {
            writeln!(
                md,
                "  - block ({}, {}): mean ΔE00 {:.3}",
                block.x, block.y, block.mean_de
            )?;
        }
        let heatmap = diffs.join(heatmap_name);
        png_io::write_srgb(
            &heatmap,
            a.width,
            a.height,
            &analysis::heatmap(&a.rgba, &result, mask),
        )?;
        Ok(result)
    };

    writeln!(md, "\n## The gate: HDR on vs the HDR-off reference\n")?;
    writeln!(
        md,
        "Pass: mean ΔE00 ≤ {} and 99th percentile ≤ {}. Pixels the shoulder changes (content that is \
         not SDR, in a frame with HDR content) are left out; without HDR content, none are.\n",
        analysis::PASS_MEAN_DE,
        analysis::PASS_P99_DE
    )?;
    for scene in scenes {
        let Some(reference) = captures.get(&(REFERENCE, scene.as_str())) else {
            continue;
        };
        for (state, _) in STATES.iter().skip(1) {
            let Some(test) = captures.get(&(*state, scene.as_str())) else {
                continue;
            };
            let mask = &masks[&(*state, scene.as_str())];
            let label = format!("{scene}: {state} vs {REFERENCE}");
            let result = compare_md(
                &mut md,
                &label,
                reference,
                test,
                &test.rgba,
                (mask.1 > 0).then_some(&mask.0[..]),
                &format!("{scene}-{state}.png"),
            )?;
            gate_results.push(result.passes());
        }
    }

    writeln!(md, "\n## SDR content brightness invariance\n")?;
    let hdr_states: Vec<&str> = STATES.iter().skip(1).map(|(s, _)| *s).collect();
    for scene in scenes {
        for pair in [(0, 1), (1, 2), (0, 2)] {
            let (sa, sb) = (hdr_states[pair.0], hdr_states[pair.1]);
            let (Some(a), Some(b)) = (
                captures.get(&(sa, scene.as_str())),
                captures.get(&(sb, scene.as_str())),
            ) else {
                continue;
            };
            let (ma, mb) = (&masks[&(sa, scene.as_str())], &masks[&(sb, scene.as_str())]);
            let mask = or_masks(&ma.0, &mb.0);
            let result = compare_md(
                &mut md,
                &format!("{scene}: {sb} vs {sa}"),
                a,
                b,
                &b.rgba,
                (ma.1 + mb.1 > 0).then_some(&mask[..]),
                &format!("{scene}-{sb}-vs-{sa}.png"),
            )?;
            invariance_results.push(result.passes());
        }
    }

    writeln!(
        md,
        "\n## For contrast: Windows' own 8-bit capture vs the reference\n"
    )?;
    writeln!(
        md,
        "What a naive capture tool produces with HDR on. Expected to fail.\n"
    )?;
    for scene in scenes {
        let Some(reference) = captures.get(&(REFERENCE, scene.as_str())) else {
            continue;
        };
        if let Some(test) = captures.get(&("hdr-mid", scene.as_str())) {
            compare_md(
                &mut md,
                &format!("{scene}: Windows 8-bit hdr-mid vs {REFERENCE}"),
                reference,
                test,
                &test.windows_rgba,
                None,
                &format!("{scene}-windows-8bit-hdr-mid.png"),
            )?;
        }
    }

    writeln!(md, "\n## How Windows placed SDR codes in scRGB\n")?;
    for scene in scenes {
        let Some(reference) = captures.get(&(REFERENCE, scene.as_str())) else {
            continue;
        };
        for state in &hdr_states {
            let Some(test) = captures.get(&(*state, scene.as_str())) else {
                continue;
            };
            let roi = Roi::full(test.width, test.height);
            let transfer = analysis::transfer(&reference.rgba, &test.raw, roi)?;
            writeln!(md, "### {scene}, {state}: against the reference\n")?;
            let table = transfer.markdown();
            if *state == "hdr-mid" {
                writeln!(md, "{table}")?;
            } else {
                // The findings only; the full table is shown for hdr-mid.
                writeln!(md, "{}", table.split("\n\n").next().unwrap_or(""))?;
            }
            writeln!(md, "Without a reference (code-grid fit):\n")?;
            writeln!(
                md,
                "{}",
                analysis::code_fit_markdown(&analysis::code_fit(&test.raw, roi)?)
            )?;
        }
    }

    writeln!(md, "## Highlights\n")?;
    let mut any_hdr = false;
    for (state, scene, c) in in_order(&captures, scenes) {
        let map = c.raw.analysis();
        if !map.has_extended() {
            continue;
        }
        any_hdr = true;
        let (_, near) = &masks[&(state, scene)];
        let gpu = Gpu::hardware(None)?;
        let texture = gpu.upload_rgba16f(c.raw.width, c.raw.height, &c.raw.data)?;
        let clipped = SdrConverter::new(&gpu)?.convert(
            &gpu,
            &texture,
            c.raw.white_scale,
            Highlights::Clip,
        )?;
        let pixels = c.raw.pixels();
        let (mut shoulder, mut clip) = (BTreeSet::new(), BTreeSet::new());
        let mut extended = 0u64;
        for (i, p) in pixels.iter().enumerate() {
            if color::peak(color::normalize([p[0], p[1], p[2]], c.raw.white_scale))
                > 1.0 + color::EXTENDED_EPSILON
            {
                extended += 1;
                shoulder.insert(&c.rgba[i * 4..i * 4 + 3]);
                clip.insert(&clipped.rgba[i * 4..i * 4 + 3]);
            }
        }
        writeln!(
            md,
            "- {state}/{scene}: frame peak {:.2}× SDR white; {extended} px above SDR white, {near} px through \
             the shoulder. Distinct output colors on the extended pixels: {} with the shoulder, {} \
             clipped.",
            map.frame_peak,
            shoulder.len(),
            clip.len()
        )?;
    }
    if !any_hdr {
        writeln!(
            md,
            "- No capture held a pixel above SDR white. If a scene showed the HDR test image, the browser \
             did not render it as HDR."
        )?;
    }

    let problems = session_problems(&captures, scenes);
    let gate = !gate_results.is_empty() && gate_results.iter().all(|p| *p);
    let invariant = !invariance_results.is_empty() && invariance_results.iter().all(|p| *p);
    writeln!(md, "\n## Verdict\n")?;
    if problems.is_empty() {
        writeln!(
            md,
            "- Session: valid (reference not HDR, distinct SDR white levels)"
        )?;
    }
    for problem in &problems {
        writeln!(md, "- Session problem: {problem}")?;
    }
    writeln!(
        md,
        "- HDR on matches HDR off: **{}** ({} of {} comparisons pass)",
        if gate { "PASS" } else { "FAIL" },
        gate_results.iter().filter(|p| **p).count(),
        gate_results.len()
    )?;
    writeln!(
        md,
        "- Independent of SDR content brightness: **{}** ({} of {} comparisons pass)",
        if invariant { "PASS" } else { "FAIL" },
        invariance_results.iter().filter(|p| **p).count(),
        invariance_results.len()
    )?;
    writeln!(
        md,
        "\n**Milestone 0 gate: {}**",
        if problems.is_empty() && gate && invariant {
            "PASS"
        } else {
            "FAIL"
        }
    )?;
    writeln!(
        md,
        "\nDifference heatmaps are in `diffs/` (red: ΔE00, full at 4; blue: left out)."
    )?;

    std::fs::write(dir.join("summary.md"), &md)?;
    Ok(md)
}

/// Default location for a new session.
pub fn default_dir() -> PathBuf {
    PathBuf::from("captures").join(format!("gate-{}", snapshot::timestamp()))
}

type Captures<'a> = std::collections::BTreeMap<(&'a str, &'a str), Capture>;

/// Captures in session order: states as captured, scenes as given.
fn in_order<'a>(
    captures: &'a Captures<'a>,
    scenes: &'a [String],
) -> impl Iterator<Item = (&'a str, &'a str, &'a Capture)> {
    STATES.iter().flat_map(move |(state, _)| {
        scenes.iter().filter_map(move |scene| {
            captures
                .get(&(*state, scene.as_str()))
                .map(|c| (*state, scene.as_str(), c))
        })
    })
}

/// Reasons the session cannot decide the gate, if any.
fn session_problems(captures: &Captures, scenes: &[String]) -> Vec<String> {
    let mut problems = Vec::new();
    let mut levels = BTreeSet::new();
    for (state, scene, c) in in_order(captures, scenes) {
        let hdr = c.raw.color_mode == ColorMode::Hdr;
        if hdr != (state != REFERENCE) {
            problems.push(format!(
                "{state}/{scene} was captured in {} mode",
                c.raw.color_mode.name()
            ));
        }
        if hdr && scene == scenes[0] {
            levels.insert(c.report["monitor"]["sdr_white_level_raw"].as_u64());
        }
    }
    if levels.len() < STATES.len() - 1 {
        problems.push(format!(
            "the HDR states share SDR white levels ({} distinct of {})",
            levels.len(),
            STATES.len() - 1
        ));
    }
    problems
}
