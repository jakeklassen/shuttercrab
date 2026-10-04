//! The Milestone 0 gate: a guided HDR-off / HDR-on capture session and the
//! report that decides it.
//!
//! Captures land in `DIR/<state>/<scene>/` (see `snapshot`), so `report`
//! can be re-run on a finished session without capturing again, and single
//! captures can be retaken with `--states`.

use crate::{
    analysis::{self, Comparison, HdrRegion, Roi},
    color::{self, ColorMode, Highlights},
    display,
    gpu::{Gpu, SdrConverter},
    png_io,
    raw::RawFrame,
    snapshot,
};
use anyhow::{Context, Result, bail, ensure};
use std::{
    collections::{BTreeMap, BTreeSet},
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
        "fixture" => "the fixture tab (fixtures/sdr-reference.html, no ?hdr), full screen".into(),
        "mixed" => {
            "the mixed tab (fixtures/sdr-reference.html?hdr, with the HDR test image), full screen"
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

/// Parse `--states`: a comma-separated subset of the gate's states.
pub fn parse_states(text: Option<&str>) -> Result<Vec<&'static str>> {
    let Some(text) = text else {
        return Ok(STATES.iter().map(|(s, _)| *s).collect());
    };
    text.split(',')
        .map(str::trim)
        .map(|name| {
            STATES
                .iter()
                .map(|(s, _)| *s)
                .find(|s| *s == name)
                .with_context(|| {
                    format!(
                        "unknown state {name:?}; the states are hdr-off, hdr-low, hdr-mid, hdr-high"
                    )
                })
        })
        .collect()
}

/// Something about a capture that suggests the wrong thing was on screen.
/// It is compared for identity with the captures of `peers` in the same state
/// only: during a session, those taken in this run, not stale ones about to
/// be retaken.
fn scene_doubt(
    state: &str,
    scene: &str,
    frame_peak: f32,
    dir: &Path,
    peers: &[String],
) -> Option<String> {
    let hdr_content = frame_peak > 1.0 + color::EXTENDED_EPSILON;
    let hdr_state = state != REFERENCE;
    if scene == "fixture" && hdr_state && hdr_content {
        return Some(format!(
            "{state}/fixture holds HDR content (peak {frame_peak:.2}× SDR white): is the ?hdr tab showing?"
        ));
    }
    if scene == "mixed" && hdr_state && !hdr_content && state != "hdr-high" {
        return Some(format!(
            "{state}/mixed holds no HDR content: is the plain tab showing instead of ?hdr?"
        ));
    }
    let this = png_io::read_rgba8(&dir.join(state).join(scene).join("capture.png")).ok()?;
    for name in peers {
        if name != scene
            && let Ok(that) = png_io::read_rgba8(&dir.join(state).join(name).join("capture.png"))
            && that == this
        {
            return Some(format!(
                "{state}/{scene} is pixel-identical to {state}/{name}: the same tab was captured twice"
            ));
        }
    }
    None
}

/// Run the guided capture session for `states`, then the report.
pub fn run(
    monitor: Option<&str>,
    scenes: &[String],
    states: &[&str],
    dir: &Path,
    delay: f64,
) -> Result<()> {
    let monitors = display::enumerate()?;
    let target = match monitor {
        Some(spec) => display::select(&monitors, Some(spec))?,
        None if monitors.len() == 1 => &monitors[0],
        None => bail!("several monitors are attached; pass --monitor (see `capture-spike list`)"),
    };
    let device = target.device_name.clone();
    std::fs::create_dir_all(dir)?;
    println!(
        "Shuttercrab Milestone 0 gate on {} ({}), saving to {}\n\
         Type q and Enter at any prompt to stop.\n\n\
         Before starting: Night light off, same resolution and scaling throughout,\n\
         browser zoom 100%, pointer parked away from the page. Captures happen\n\
         {delay} s after you press Enter, so you can switch to the page.\n",
        device,
        target.friendly_name,
        dir.display()
    );

    let mut white_levels: Vec<(String, u32)> = Vec::new();
    for (state, slider) in STATES.iter().filter(|(s, _)| states.contains(s)) {
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
                white_levels.push((state.to_string(), level));
                println!(
                    "SDR white level {level} ({:.0} nits).",
                    color::sdr_white_nits(level)
                );
            }
            break;
        }
        for (index, scene) in scenes.iter().enumerate() {
            let taken = &scenes[..index];
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
                match snapshot::take(Some(&device), &out, Highlights::Tonemap) {
                    Ok(shot) => {
                        println!(
                            "\x07Captured {state}/{scene}: {}, S = {}, frame peak {:.2}",
                            shot.color_mode.name(),
                            shot.white_scale,
                            shot.frame_peak
                        );
                        if let Some(doubt) = scene_doubt(state, scene, shot.frame_peak, dir, taken)
                        {
                            println!("Check: {doubt}");
                            if prompt("Enter to retake it, or type keep to keep it: ")? != "keep" {
                                continue;
                            }
                        }
                        println!();
                        break;
                    }
                    Err(e) => println!("\x07Capture failed: {e:#}\nTrying again."),
                }
            }
        }
    }
    let summary = report(dir, None)?;
    println!("{summary}");
    println!("Written to {}", dir.join("summary.md").display());
    Ok(())
}

struct Capture {
    /// The product output, re-derived from `raw` with the current transform
    /// for HDR and WCG captures.
    rgba: Vec<u8>,
    windows_rgba: Vec<u8>,
    raw: RawFrame,
    width: u32,
    height: u32,
    report: serde_json::Value,
    /// Left out of SDR comparisons (see `analysis::hdr_region`).
    region: HdrRegion,
    frame_peak: f32,
}

fn load(dir: &Path, gpu: &Gpu, converter: &SdrConverter) -> Result<Capture> {
    let (width, height, stored) = png_io::read_rgba8(&dir.join("capture.png"))?;
    let (_, _, windows_rgba) = png_io::read_rgba8(&dir.join("windows-8bit.png"))?;
    let raw = RawFrame::read(&dir.join("source.fp16"))?;
    let report = serde_json::from_str(&std::fs::read_to_string(dir.join("report.json"))?)?;
    let texture = gpu.upload_rgba16f(raw.width, raw.height, &raw.data)?;
    let sdr = converter.convert(gpu, &texture, raw.white_scale, Highlights::Tonemap)?;
    let rgba = match raw.color_mode {
        // The SDR product path is the 8-bit capture, not a transform.
        ColorMode::Sdr => stored,
        ColorMode::Wcg | ColorMode::Hdr => sdr.rgba,
    };
    let region = analysis::hdr_region(&raw);
    Ok(Capture {
        rgba,
        windows_rgba,
        width,
        height,
        report,
        region,
        frame_peak: sdr.frame_peak,
        raw,
    })
}

/// The capture's HDR region as a comparison mask, if it has one.
fn region_mask(c: &Capture) -> Option<&[bool]> {
    (c.region.pixels > 0).then_some(&c.region.mask[..])
}

fn or_masks(a: &[bool], b: &[bool]) -> Vec<bool> {
    a.iter().zip(b).map(|(x, y)| *x || *y).collect()
}

/// Scenes present in a session directory, `fixture` and `mixed` first.
fn discover_scenes(dir: &Path) -> Vec<String> {
    let mut found = BTreeSet::new();
    for (state, _) in STATES {
        if let Ok(entries) = std::fs::read_dir(dir.join(state)) {
            for entry in entries.flatten().filter(|e| e.path().is_dir()) {
                found.insert(entry.file_name().to_string_lossy().into_owned());
            }
        }
    }
    let mut scenes: Vec<String> = ["fixture", "mixed"]
        .into_iter()
        .filter(|s| found.remove(*s))
        .map(String::from)
        .collect();
    scenes.extend(found);
    scenes
}

/// Analyse a finished gate directory and write `summary.md`. With no
/// `scenes`, every scene in the directory is analysed.
#[expect(
    clippy::too_many_lines,
    reason = "writes summary.md section by section, in the order it reads"
)]
pub fn report(dir: &Path, scenes: Option<&[String]>) -> Result<String> {
    let scenes: Vec<String> = match scenes {
        Some(s) => s.to_vec(),
        None => discover_scenes(dir),
    };
    let scenes = &scenes[..];
    ensure!(!scenes.is_empty(), "no captures in {}", dir.display());
    let diffs = dir.join("diffs");
    std::fs::create_dir_all(&diffs)?;
    let mut md = String::new();
    let mut gate_results: Vec<bool> = Vec::new();
    let mut invariance_results: Vec<bool> = Vec::new();

    writeln!(md, "# Shuttercrab Milestone 0 gate\n")?;
    writeln!(
        md,
        "Session `{}`, analysed with transform `{}` (capture-spike {}). HDR captures are \
         re-converted from their saved FP16 source with this transform.\n",
        dir.file_name().map_or("?".into(), |n| n.to_string_lossy()),
        color::TRANSFORM_VERSION,
        env!("CARGO_PKG_VERSION")
    )?;

    let gpu = Gpu::hardware(None)?;
    let converter = SdrConverter::new(&gpu)?;
    let mut captures: Captures = BTreeMap::new();
    for (state, _) in STATES {
        for scene in scenes {
            let path = dir.join(state).join(scene);
            match load(&path, &gpu, &converter) {
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
        "| state | scene | mode | SDR white level | S | frame peak | left out of SDR comparisons |"
    )?;
    writeln!(md, "|---|---|---|---:|---:|---:|---|")?;
    for (state, scene, c) in in_order(&captures, scenes) {
        let mon = &c.report["monitor"];
        let left_out = match c.region.bounds {
            Some(b) => format!(
                "{} px, within ({}, {}) {}×{}",
                c.region.pixels, b.x, b.y, b.width, b.height
            ),
            None => "none".into(),
        };
        writeln!(
            md,
            "| {state} | {scene} | {} | {} | {} | {:.3} | {left_out} |",
            mon["color_mode"].as_str().unwrap_or("?"),
            mon["sdr_white_level_raw"],
            c.raw.white_scale,
            c.frame_peak,
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

    let compare_md = |md: &mut String,
                      label: &str,
                      a: &Capture,
                      b_rgba: &[u8],
                      mask: Option<&[bool]>,
                      heatmap_name: &str|
     -> Result<Comparison> {
        ensure!(
            a.rgba.len() == b_rgba.len(),
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
        png_io::write_srgb(
            &diffs.join(heatmap_name),
            a.width,
            a.height,
            &analysis::heatmap(&a.rgba, &result, mask),
        )?;
        Ok(result)
    };

    writeln!(md, "\n## The gate: HDR on vs the HDR-off reference\n")?;
    writeln!(
        md,
        "Pass: mean ΔE00 ≤ {}, 99th percentile ≤ {}, and no 64×64 block averaging over {}. In a frame \
         with HDR content, the region of that content is left out (see the table above): there the \
         reference shows the application's own tone mapping. Without HDR content nothing is left out.\n",
        analysis::PASS_MEAN_DE,
        analysis::PASS_P99_DE,
        analysis::PASS_BLOCK_DE
    )?;
    for scene in scenes {
        let Some(reference) = captures.get(&(REFERENCE, scene.as_str())) else {
            continue;
        };
        for (state, _) in STATES.iter().skip(1) {
            let Some(test) = captures.get(&(*state, scene.as_str())) else {
                continue;
            };
            let result = compare_md(
                &mut md,
                &format!("{scene}: {state} vs {REFERENCE}"),
                reference,
                &test.rgba,
                region_mask(test),
                &format!("{scene}-{state}.png"),
            )?;
            gate_results.push(result.passes());
        }
    }

    writeln!(md, "\n## SDR content brightness invariance\n")?;
    let hdr_states: Vec<&str> = STATES.iter().skip(1).map(|(s, _)| *s).collect();
    for scene in scenes {
        for (ia, ib) in [(0, 1), (1, 2), (0, 2)] {
            let (sa, sb) = (hdr_states[ia], hdr_states[ib]);
            let (Some(a), Some(b)) = (
                captures.get(&(sa, scene.as_str())),
                captures.get(&(sb, scene.as_str())),
            ) else {
                continue;
            };
            let mask = or_masks(&a.region.mask, &b.region.mask);
            let any = a.region.pixels + b.region.pixels > 0;
            let result = compare_md(
                &mut md,
                &format!("{scene}: {sb} vs {sa}"),
                a,
                &b.rgba,
                any.then_some(&mask[..]),
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
                &test.windows_rgba,
                region_mask(test),
                &format!("{scene}-windows-8bit-hdr-mid.png"),
            )?;
        }
    }

    writeln!(md, "\n## How Windows placed SDR codes in scRGB\n")?;
    writeln!(
        md,
        "Measured against the reference, outside any HDR region.\n"
    )?;
    for scene in scenes {
        let Some(reference) = captures.get(&(REFERENCE, scene.as_str())) else {
            continue;
        };
        for state in &hdr_states {
            let Some(test) = captures.get(&(*state, scene.as_str())) else {
                continue;
            };
            let roi = Roi::full(test.width, test.height);
            let transfer = analysis::transfer(&reference.rgba, &test.raw, roi, region_mask(test))?;
            writeln!(md, "### {scene}, {state}\n")?;
            let table = transfer.markdown();
            if *state == "hdr-mid" {
                writeln!(md, "{table}")?;
            } else {
                // The findings only; the full table is shown for hdr-mid.
                writeln!(md, "{}\n", table.split("\n\n").next().unwrap_or(""))?;
            }
            writeln!(md, "Without a reference (code-grid fit, whole frame):\n")?;
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
        let analysis = c.raw.analysis();
        if !analysis.has_extended() {
            continue;
        }
        any_hdr = true;
        let texture = gpu.upload_rgba16f(c.raw.width, c.raw.height, &c.raw.data)?;
        let clipped = converter.convert(&gpu, &texture, c.raw.white_scale, Highlights::Clip)?;
        let (mut mapped, mut clip) = (BTreeSet::new(), BTreeSet::new());
        let (mut extended, mut in_regions) = (0u64, 0u64);
        // How close the HDR content comes to the application's own rendering
        // with HDR off, where the session has that reference.
        let reference = captures.get(&(REFERENCE, scene));
        let (mut de_sum, mut de_n) = (0.0f64, 0u64);
        let mut cache = std::collections::HashMap::new();
        for (i, p) in c.raw.pixels().iter().enumerate() {
            let (x, y) = (i as u32 % c.raw.width, i as u32 / c.raw.width);
            if analysis.region_at(x, y).is_some() {
                in_regions += 1;
                if let Some(r) = reference {
                    let a = [r.rgba[i * 4], r.rgba[i * 4 + 1], r.rgba[i * 4 + 2]];
                    let b = [c.rgba[i * 4], c.rgba[i * 4 + 1], c.rgba[i * 4 + 2]];
                    de_sum += *cache.entry((a, b)).or_insert_with(|| {
                        analysis::delta_e2000(analysis::srgb8_to_lab(a), analysis::srgb8_to_lab(b))
                    });
                    de_n += 1;
                }
            }
            if color::peak(color::normalize([p[0], p[1], p[2]], c.raw.white_scale))
                > 1.0 + color::EXTENDED_EPSILON
            {
                extended += 1;
                mapped.insert(&c.rgba[i * 4..i * 4 + 3]);
                clip.insert(&clipped.rgba[i * 4..i * 4 + 3]);
            }
        }
        let regions: Vec<String> = analysis
            .regions
            .iter()
            .map(|r| {
                format!(
                    "({}, {})–({}, {}) peak {:.2}",
                    r.x0, r.y0, r.x1, r.y1, r.peak
                )
            })
            .collect();
        writeln!(
            md,
            "- {state}/{scene}: frame peak {:.2}× SDR white; {extended} px above SDR white. HDR regions: {}. \
             Distinct output colors on the extended pixels: {} tone mapped, {} clipped.{}",
            analysis.frame_peak,
            if regions.is_empty() {
                "none".into()
            } else {
                regions.join(", ")
            },
            mapped.len(),
            clip.len(),
            if de_n > 0 {
                format!(
                    " Inside the regions vs the application's own HDR-off rendering: mean ΔE00 {:.2} over {in_regions} px.",
                    de_sum / de_n as f64
                )
            } else {
                String::new()
            }
        )?;
    }
    if !any_hdr {
        writeln!(
            md,
            "- No capture held a pixel above SDR white. If a scene showed the HDR test image, the browser \
             did not render it as HDR."
        )?;
    }

    let problems = session_problems(&captures, scenes, dir);
    let gate = !gate_results.is_empty() && gate_results.iter().all(|p| *p);
    let invariant = !invariance_results.is_empty() && invariance_results.iter().all(|p| *p);
    writeln!(md, "\n## Verdict\n")?;
    if problems.is_empty() {
        writeln!(
            md,
            "- Session: valid (reference not HDR, distinct SDR white levels, scenes as labelled)"
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
        if !problems.is_empty() {
            "UNDECIDED (fix the session problems above and re-run)"
        } else if gate && invariant {
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

type Captures<'a> = BTreeMap<(&'a str, &'a str), Capture>;

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
fn session_problems(captures: &Captures, scenes: &[String], dir: &Path) -> Vec<String> {
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
        if let Some(doubt) = scene_doubt(state, scene, c.frame_peak, dir, scenes) {
            problems.push(doubt);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn states_parse() {
        assert_eq!(parse_states(None).unwrap().len(), 4);
        assert_eq!(
            parse_states(Some("hdr-low, hdr-mid")).unwrap(),
            ["hdr-low", "hdr-mid"]
        );
        assert!(parse_states(Some("hdr-medium")).is_err());
    }
}
