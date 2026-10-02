//! Monitor enumeration: bounds, DPI, Advanced Color / HDR state, and the
//! SDR white level, for every monitor attached to the desktop.
//!
//! DXGI supplies the `HMONITOR`, adapter, physical desktop bounds and output
//! color space. DisplayConfig supplies the Advanced Color mode and the SDR
//! white level. The two are joined through the GDI source name.

use crate::color::{self, ColorMode};
use crate::gpu::wide_to_string;
use anyhow::{Context, Result, bail, ensure};
use serde_json::{Value, json};
use windows::{
    Win32::{
        Devices::Display::*,
        Foundation::{ERROR_INSUFFICIENT_BUFFER, POINT},
        Graphics::{
            Dxgi::{Common::*, *},
            Gdi::{GetMonitorInfoW, MONITOR_DEFAULTTONULL, MONITORINFO, MonitorFromPoint},
        },
        UI::{
            HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI},
            WindowsAndMessaging::{GetCursorPos, MONITORINFOF_PRIMARY},
        },
    },
    core::Interface,
};

/// A rectangle in physical pixels, in virtual-desktop coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

pub struct Monitor {
    /// GDI device name, e.g. `\\.\DISPLAY1`. Stable across HDR toggles,
    /// unlike enumeration order.
    pub device_name: String,
    pub friendly_name: String,
    pub hmonitor: windows::Win32::Graphics::Gdi::HMONITOR,
    pub adapter: IDXGIAdapter1,
    pub adapter_name: String,
    pub bounds: Rect,
    pub primary: bool,
    pub dpi: u32,
    pub color_mode: ColorMode,
    /// Which DisplayConfig query decided `color_mode`.
    pub color_mode_source: &'static str,
    pub advanced_color_supported: bool,
    pub hdr_supported: Option<bool>,
    pub hdr_user_enabled: Option<bool>,
    pub bits_per_color_channel: u32,
    /// Raw `DISPLAYCONFIG_SDR_WHITE_LEVEL::SDRWhiteLevel` (1000 = 80 nits).
    pub sdr_white_level: Option<u32>,
    pub dxgi_color_space: DXGI_COLOR_SPACE_TYPE,
    pub dxgi_max_luminance: f32,
}

impl Monitor {
    pub fn scale_factor(&self) -> f32 {
        self.dpi as f32 / 96.0
    }

    pub fn advanced_color_enabled(&self) -> bool {
        self.color_mode.advanced_color()
    }

    pub fn hdr_enabled(&self) -> bool {
        self.color_mode == ColorMode::Hdr
    }

    pub fn sdr_white_nits(&self) -> Option<f32> {
        self.sdr_white_level.map(color::sdr_white_nits)
    }

    /// `S`, the scRGB value of SDR white on this monitor.
    pub fn white_scale(&self) -> Result<f32> {
        color::white_scale(self.color_mode, self.sdr_white_level)
    }

    /// Disagreements between the two sources of truth, worth reporting.
    pub fn warnings(&self) -> Vec<String> {
        let pq = self.dxgi_color_space == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;
        let mut warnings = Vec::new();
        if self.hdr_enabled() != pq {
            warnings.push(format!(
                "DisplayConfig reports {} but the DXGI output color space is {}",
                self.color_mode.name(),
                color_space_name(self.dxgi_color_space)
            ));
        }
        warnings
    }

    /// The state a capture depends on. If it changes mid-capture, the frame
    /// and the metadata used to convert it may not belong together.
    pub fn state(&self) -> (ColorMode, Option<u32>, Rect) {
        (self.color_mode, self.sdr_white_level, self.bounds)
    }

    pub fn to_json(&self) -> Value {
        json!({
            "device_name": self.device_name,
            "friendly_name": self.friendly_name,
            "adapter": self.adapter_name,
            "bounds": {
                "x": self.bounds.x,
                "y": self.bounds.y,
                "width": self.bounds.width,
                "height": self.bounds.height,
            },
            "primary": self.primary,
            "dpi": self.dpi,
            "scale_factor": self.scale_factor(),
            "color_mode": self.color_mode.name(),
            "color_mode_source": self.color_mode_source,
            "advanced_color_supported": self.advanced_color_supported,
            "advanced_color_enabled": self.advanced_color_enabled(),
            "hdr_supported": self.hdr_supported,
            "hdr_user_enabled": self.hdr_user_enabled,
            "hdr_enabled": self.hdr_enabled(),
            "bits_per_color_channel": self.bits_per_color_channel,
            "sdr_white_level_raw": self.sdr_white_level,
            "sdr_white_level_nits": self.sdr_white_nits(),
            "white_scale": self.white_scale().ok(),
            "dxgi_color_space": color_space_name(self.dxgi_color_space),
            "dxgi_max_luminance_nits": self.dxgi_max_luminance,
            "warnings": self.warnings(),
        })
    }

    pub fn describe(&self) -> String {
        let b = self.bounds;
        let white = match (self.sdr_white_level, self.white_scale()) {
            (Some(raw), Ok(s)) => {
                format!("{raw} ({:.0} nits), S = {s}", color::sdr_white_nits(raw))
            }
            (Some(raw), Err(_)) => format!("{raw} ({:.0} nits)", color::sdr_white_nits(raw)),
            (None, Ok(s)) => format!("not reported, S = {s}"),
            (None, Err(_)) => "not reported".into(),
        };
        let mut text = format!(
            "{}  {}{}\n    bounds {}x{} at ({}, {}), {} DPI ({:.0}%), adapter {}\n    \
             color mode {} [{}], Advanced Color {}, HDR {}\n    \
             SDR white level {}\n    DXGI {} , {} bpc, peak {:.0} nits",
            self.device_name,
            self.friendly_name,
            if self.primary { " (primary)" } else { "" },
            b.width,
            b.height,
            b.x,
            b.y,
            self.dpi,
            self.scale_factor() * 100.0,
            self.adapter_name,
            self.color_mode.name(),
            self.color_mode_source,
            on_off(self.advanced_color_enabled()),
            on_off(self.hdr_enabled()),
            white,
            color_space_name(self.dxgi_color_space),
            self.bits_per_color_channel,
            self.dxgi_max_luminance,
        );
        for warning in self.warnings() {
            text.push_str(&format!("\n    warning: {warning}"));
        }
        text
    }
}

fn on_off(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

pub fn color_space_name(space: DXGI_COLOR_SPACE_TYPE) -> String {
    match space {
        DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709 => "sRGB (G22 P709)".into(),
        DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 => "HDR10 (G2084 P2020)".into(),
        DXGI_COLOR_SPACE_RGB_FULL_G10_NONE_P709 => "scRGB (G10 P709)".into(),
        other => format!("DXGI_COLOR_SPACE_TYPE({})", other.0),
    }
}

/// Decide the compositor mode. The Windows 11 24H2 query reports it
/// directly; older builds only say whether Advanced Color is on, and HDR is
/// then told apart from Advanced Color SDR by the output's PQ color space.
pub fn decide_color_mode(
    active_color_mode: Option<i32>,
    legacy_advanced_color_enabled: bool,
    dxgi_color_space: DXGI_COLOR_SPACE_TYPE,
) -> Result<ColorMode> {
    Ok(match active_color_mode {
        Some(0) => ColorMode::Sdr,
        Some(1) => ColorMode::Wcg,
        Some(2) => ColorMode::Hdr,
        Some(other) => bail!("unknown DISPLAYCONFIG_ADVANCED_COLOR_MODE {other}"),
        None if !legacy_advanced_color_enabled => ColorMode::Sdr,
        None if dxgi_color_space == DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020 => ColorMode::Hdr,
        None => ColorMode::Wcg,
    })
}

/// `DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO_2` from the Windows 11 24H2 SDK
/// (wingdi.h, 10.0.26100). windows 0.62 predates it.
#[repr(C)]
struct AdvancedColorInfo2 {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    flags: u32,
    color_encoding: i32,
    bits_per_color_channel: u32,
    active_color_mode: i32,
}

const GET_ADVANCED_COLOR_INFO_2: DISPLAYCONFIG_DEVICE_INFO_TYPE =
    DISPLAYCONFIG_DEVICE_INFO_TYPE(15);

fn header<T>(
    kind: DISPLAYCONFIG_DEVICE_INFO_TYPE,
    adapter: windows::Win32::Foundation::LUID,
    id: u32,
) -> DISPLAYCONFIG_DEVICE_INFO_HEADER {
    DISPLAYCONFIG_DEVICE_INFO_HEADER {
        r#type: kind,
        size: size_of::<T>() as u32,
        adapterId: adapter,
        id,
    }
}

fn active_paths() -> Result<Vec<DISPLAYCONFIG_PATH_INFO>> {
    // The topology can change between sizing and querying; retry a few times.
    for _ in 0..5 {
        let (mut path_count, mut mode_count) = (0u32, 0u32);
        unsafe {
            GetDisplayConfigBufferSizes(QDC_ONLY_ACTIVE_PATHS, &mut path_count, &mut mode_count)
        }
        .ok()
        .context("GetDisplayConfigBufferSizes failed")?;
        let mut paths = vec![DISPLAYCONFIG_PATH_INFO::default(); path_count as usize];
        let mut modes = vec![DISPLAYCONFIG_MODE_INFO::default(); mode_count as usize];
        let status = unsafe {
            QueryDisplayConfig(
                QDC_ONLY_ACTIVE_PATHS,
                &mut path_count,
                paths.as_mut_ptr(),
                &mut mode_count,
                modes.as_mut_ptr(),
                None,
            )
        };
        if status == ERROR_INSUFFICIENT_BUFFER {
            continue;
        }
        status.ok().context("QueryDisplayConfig failed")?;
        paths.truncate(path_count as usize);
        return Ok(paths);
    }
    bail!("the display topology kept changing; try again")
}

fn device_info<T>(packet: &mut T) -> i32 {
    unsafe { DisplayConfigGetDeviceInfo(packet as *mut T as *mut DISPLAYCONFIG_DEVICE_INFO_HEADER) }
}

pub fn enumerate() -> Result<Vec<Monitor>> {
    let paths = active_paths()?;
    let factory: IDXGIFactory1 = unsafe { CreateDXGIFactory1()? };
    let mut monitors = Vec::new();
    for adapter_index in 0.. {
        let adapter = match unsafe { factory.EnumAdapters1(adapter_index) } {
            Ok(adapter) => adapter,
            Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
            Err(e) => return Err(e.into()),
        };
        let adapter_name = wide_to_string(&unsafe { adapter.GetDesc1()? }.Description);
        for output_index in 0.. {
            let output = match unsafe { adapter.EnumOutputs(output_index) } {
                Ok(output) => output,
                Err(e) if e.code() == DXGI_ERROR_NOT_FOUND => break,
                Err(e) => return Err(e.into()),
            };
            let desc = unsafe { output.cast::<IDXGIOutput6>()?.GetDesc1()? };
            if !desc.AttachedToDesktop.as_bool() {
                continue;
            }
            let device_name = wide_to_string(&desc.DeviceName);
            monitors.push(describe_output(
                &paths,
                &adapter,
                &adapter_name,
                &device_name,
                &desc,
            )?);
        }
    }
    ensure!(
        !monitors.is_empty(),
        "no monitors are attached to the desktop"
    );
    Ok(monitors)
}

/// The display path whose source is the GDI device `device_name`.
fn path_for<'a>(
    paths: &'a [DISPLAYCONFIG_PATH_INFO],
    device_name: &str,
) -> Result<&'a DISPLAYCONFIG_PATH_INFO> {
    let mut matching = Vec::new();
    for path in paths {
        let mut source = DISPLAYCONFIG_SOURCE_DEVICE_NAME {
            header: header::<DISPLAYCONFIG_SOURCE_DEVICE_NAME>(
                DISPLAYCONFIG_DEVICE_INFO_GET_SOURCE_NAME,
                path.sourceInfo.adapterId,
                path.sourceInfo.id,
            ),
            ..Default::default()
        };
        if device_info(&mut source) == 0 && wide_to_string(&source.viewGdiDeviceName) == device_name
        {
            matching.push(path);
        }
    }
    ensure!(
        matching.len() == 1,
        "{device_name} maps to {} display paths; cloned displays are not supported, use an extended desktop",
        matching.len()
    );
    Ok(matching[0])
}

/// `DISPLAYCONFIG_SET_HDR_STATE` from the Windows 11 24H2 SDK (wingdi.h,
/// 10.0.26100): bit 0 turns HDR on. windows 0.62 predates it.
#[repr(C)]
struct SetHdrState {
    header: DISPLAYCONFIG_DEVICE_INFO_HEADER,
    value: u32,
}

const SET_HDR_STATE: DISPLAYCONFIG_DEVICE_INFO_TYPE = DISPLAYCONFIG_DEVICE_INFO_TYPE(16);

/// Turn HDR on or off for the monitor `device_name` (`\\.\DISPLAY2`), as
/// the Settings app's "Use HDR" switch does. For tests of display changes.
pub fn set_hdr(device_name: &str, on: bool) -> Result<()> {
    let paths = active_paths()?;
    let target = path_for(&paths, device_name)?.targetInfo;
    let status = if windows_build() >= 26100 {
        let packet = SetHdrState {
            header: header::<SetHdrState>(SET_HDR_STATE, target.adapterId, target.id),
            value: on as u32,
        };
        // SAFETY: the packet starts with its header, whose size covers it.
        unsafe { DisplayConfigSetDeviceInfo((&raw const packet).cast()) }
    } else {
        let packet = DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE {
            header: header::<DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE>(
                DISPLAYCONFIG_DEVICE_INFO_SET_ADVANCED_COLOR_STATE,
                target.adapterId,
                target.id,
            ),
            Anonymous: DISPLAYCONFIG_SET_ADVANCED_COLOR_STATE_0 { value: on as u32 },
        };
        // SAFETY: as above.
        unsafe { DisplayConfigSetDeviceInfo((&raw const packet).cast()) }
    };
    ensure!(
        status == 0,
        "turning HDR {} on {device_name} failed ({status})",
        on_off(on)
    );
    Ok(())
}

fn describe_output(
    paths: &[DISPLAYCONFIG_PATH_INFO],
    adapter: &IDXGIAdapter1,
    adapter_name: &str,
    device_name: &str,
    desc: &DXGI_OUTPUT_DESC1,
) -> Result<Monitor> {
    let target = path_for(paths, device_name)?.targetInfo;

    let mut name = DISPLAYCONFIG_TARGET_DEVICE_NAME {
        header: header::<DISPLAYCONFIG_TARGET_DEVICE_NAME>(
            DISPLAYCONFIG_DEVICE_INFO_GET_TARGET_NAME,
            target.adapterId,
            target.id,
        ),
        ..Default::default()
    };
    let friendly_name = if device_info(&mut name) == 0 {
        wide_to_string(&name.monitorFriendlyDeviceName)
    } else {
        String::new()
    };

    let mut legacy = DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO {
        header: header::<DISPLAYCONFIG_GET_ADVANCED_COLOR_INFO>(
            DISPLAYCONFIG_DEVICE_INFO_GET_ADVANCED_COLOR_INFO,
            target.adapterId,
            target.id,
        ),
        ..Default::default()
    };
    let status = device_info(&mut legacy);
    ensure!(
        status == 0,
        "reading Advanced Color state of {device_name} failed ({status})"
    );
    let legacy_flags = unsafe { legacy.Anonymous.value };

    let mut info2 = AdvancedColorInfo2 {
        header: header::<AdvancedColorInfo2>(
            GET_ADVANCED_COLOR_INFO_2,
            target.adapterId,
            target.id,
        ),
        flags: 0,
        color_encoding: 0,
        bits_per_color_channel: 0,
        active_color_mode: 0,
    };
    let info2 = (device_info(&mut info2) == 0).then_some(info2);

    let color_mode = decide_color_mode(
        info2.as_ref().map(|i| i.active_color_mode),
        legacy_flags & 0b10 != 0,
        desc.ColorSpace,
    )?;

    let mut white = DISPLAYCONFIG_SDR_WHITE_LEVEL {
        header: header::<DISPLAYCONFIG_SDR_WHITE_LEVEL>(
            DISPLAYCONFIG_DEVICE_INFO_GET_SDR_WHITE_LEVEL,
            target.adapterId,
            target.id,
        ),
        SDRWhiteLevel: 0,
    };
    let sdr_white_level = (device_info(&mut white) == 0).then_some(white.SDRWhiteLevel);

    let hmonitor = desc.Monitor;
    let mut info = MONITORINFO {
        cbSize: size_of::<MONITORINFO>() as u32,
        ..Default::default()
    };
    let primary = unsafe { GetMonitorInfoW(hmonitor, &mut info) }.as_bool()
        && info.dwFlags & MONITORINFOF_PRIMARY != 0;
    let (mut dpi_x, mut dpi_y) = (96u32, 96u32);
    unsafe { GetDpiForMonitor(hmonitor, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
        .with_context(|| format!("GetDpiForMonitor failed for {device_name}"))?;

    let r = desc.DesktopCoordinates;
    Ok(Monitor {
        device_name: device_name.to_owned(),
        friendly_name,
        hmonitor,
        adapter: adapter.clone(),
        adapter_name: adapter_name.to_owned(),
        bounds: Rect {
            x: r.left,
            y: r.top,
            width: (r.right - r.left) as u32,
            height: (r.bottom - r.top) as u32,
        },
        primary,
        dpi: dpi_x,
        color_mode,
        color_mode_source: if info2.is_some() {
            "ADVANCED_COLOR_INFO_2"
        } else {
            "ADVANCED_COLOR_INFO + DXGI"
        },
        advanced_color_supported: match &info2 {
            Some(i) => i.flags & 1 != 0,
            None => legacy_flags & 1 != 0,
        },
        hdr_supported: info2.as_ref().map(|i| i.flags & (1 << 4) != 0),
        hdr_user_enabled: info2.as_ref().map(|i| i.flags & (1 << 5) != 0),
        bits_per_color_channel: match &info2 {
            Some(i) => i.bits_per_color_channel,
            None => legacy.bitsPerColorChannel,
        },
        sdr_white_level,
        dxgi_color_space: desc.ColorSpace,
        dxgi_max_luminance: desc.MaxLuminance,
    })
}

/// Pick a monitor by list index, by device name (`DISPLAY2` or
/// `\\.\DISPLAY2`), or, with no spec, the one under the pointer.
pub fn select<'a>(monitors: &'a [Monitor], spec: Option<&str>) -> Result<&'a Monitor> {
    match spec {
        Some(spec) => {
            if let Ok(index) = spec.parse::<usize>() {
                return monitors.get(index).with_context(|| {
                    format!("there is no monitor {index}; run `capture-spike list`")
                });
            }
            let wanted = spec.trim_start_matches(r"\\.\").to_ascii_uppercase();
            monitors
                .iter()
                .find(|m| {
                    m.device_name
                        .trim_start_matches(r"\\.\")
                        .eq_ignore_ascii_case(&wanted)
                })
                .with_context(|| format!("no monitor is named {spec}; run `capture-spike list`"))
        }
        None => {
            let mut point = POINT::default();
            unsafe { GetCursorPos(&mut point)? };
            let under = unsafe { MonitorFromPoint(point, MONITOR_DEFAULTTONULL) };
            monitors
                .iter()
                .find(|m| m.hmonitor == under)
                .context("no monitor is under the pointer")
        }
    }
}

/// Look a monitor up again by device name, after the display state may have changed.
pub fn find<'a>(monitors: &'a [Monitor], device_name: &str) -> Result<&'a Monitor> {
    monitors
        .iter()
        .find(|m| m.device_name == device_name)
        .with_context(|| format!("{device_name} is no longer attached"))
}

/// Windows build number, from the kernel rather than the manifest-shimmed API.
pub fn windows_build() -> u32 {
    use windows::{
        Wdk::System::SystemServices::RtlGetVersion,
        Win32::System::SystemInformation::OSVERSIONINFOW,
    };
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: size_of::<OSVERSIONINFOW>() as u32,
        ..Default::default()
    };
    match unsafe { RtlGetVersion(&mut info) }.ok() {
        Ok(()) => info.dwBuildNumber,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SRGB: DXGI_COLOR_SPACE_TYPE = DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709;
    const PQ: DXGI_COLOR_SPACE_TYPE = DXGI_COLOR_SPACE_RGB_FULL_G2084_NONE_P2020;

    #[test]
    fn the_24h2_query_decides_the_mode() {
        assert_eq!(
            decide_color_mode(Some(0), true, PQ).unwrap(),
            ColorMode::Sdr
        );
        assert_eq!(
            decide_color_mode(Some(1), true, SRGB).unwrap(),
            ColorMode::Wcg
        );
        assert_eq!(
            decide_color_mode(Some(2), true, PQ).unwrap(),
            ColorMode::Hdr
        );
        assert!(decide_color_mode(Some(7), true, PQ).is_err());
    }

    #[test]
    fn advanced_color_alone_does_not_mean_hdr() {
        assert_eq!(
            decide_color_mode(None, false, SRGB).unwrap(),
            ColorMode::Sdr
        );
        assert_eq!(decide_color_mode(None, true, SRGB).unwrap(), ColorMode::Wcg);
        assert_eq!(decide_color_mode(None, true, PQ).unwrap(), ColorMode::Hdr);
    }

    #[test]
    fn info2_layout_matches_the_sdk() {
        // header (20) + flags + colorEncoding + bitsPerColorChannel + activeColorMode.
        assert_eq!(size_of::<DISPLAYCONFIG_DEVICE_INFO_HEADER>(), 20);
        assert_eq!(size_of::<AdvancedColorInfo2>(), 36);
        // header (20) + the enableHdr bit field.
        assert_eq!(size_of::<SetHdrState>(), 24);
    }
}
