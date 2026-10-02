//! Shuttercrab's icon, drawn in code: a dark rounded square with coral
//! selection corners at the top left and bottom right, as around the
//! wordmark. Drawing it keeps the tray sharp at any DPI without shipping
//! image files.

use anyhow::{Context, Result, bail};
use windows::Win32::{
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
        DeleteObject, HGDIOBJ,
    },
    UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO},
};

/// The tile, #2A2E36.
const TILE: [f32; 3] = [0x2A as f32, 0x2E as f32, 0x36 as f32];
/// The corners, Shuttercrab's coral, #E8603C.
const CORAL: [f32; 3] = [0xE8 as f32, 0x60 as f32, 0x3C as f32];

/// The icon as tightly packed straight-alpha RGBA, `size` × `size`.
pub fn rgba(size: u32) -> Vec<u8> {
    let s = size as f32;
    let radius = s * 0.22;
    // Selection corners: arms of this length and thickness, inset from the edge.
    let inset = s * 0.22;
    let arm = s * 0.30;
    let thick = (s * 0.09).max(1.0);
    let inside_square = |x: f32, y: f32| {
        let (cx, cy) = (x.clamp(radius, s - radius), y.clamp(radius, s - radius));
        (x - cx).powi(2) + (y - cy).powi(2) <= radius * radius
    };
    let on_corner = |x: f32, y: f32| {
        [(inset, inset, 1.0, 1.0), (s - inset, s - inset, -1.0, -1.0)]
            .iter()
            .any(|&(ox, oy, dx, dy): &(f32, f32, f32, f32)| {
                let (u, v) = ((x - ox) * dx, (y - oy) * dy);
                (0.0..arm).contains(&u) && (0.0..thick).contains(&v)
                    || (0.0..thick).contains(&u) && (0.0..arm).contains(&v)
            })
    };
    // 4×4 supersampling for smooth edges at tray sizes.
    const N: u32 = 4;
    let mut out = Vec::with_capacity((size * size * 4) as usize);
    for py in 0..size {
        for px in 0..size {
            let (mut cover, mut marked) = (0u32, 0u32);
            for sy in 0..N {
                for sx in 0..N {
                    let x = px as f32 + (sx as f32 + 0.5) / N as f32;
                    let y = py as f32 + (sy as f32 + 0.5) / N as f32;
                    if inside_square(x, y) {
                        cover += 1;
                        marked += u32::from(on_corner(x, y));
                    }
                }
            }
            let alpha = cover as f32 / (N * N) as f32;
            let m = if cover > 0 {
                marked as f32 / cover as f32
            } else {
                0.0
            };
            let c: [u8; 3] =
                std::array::from_fn(|i| (TILE[i] + (CORAL[i] - TILE[i]) * m).round() as u8);
            out.extend_from_slice(&[c[0], c[1], c[2], (alpha * 255.0).round() as u8]);
        }
    }
    out
}

/// The sizes Windows asks an application icon for, from the small title-bar
/// icon to the large Start menu tile.
pub const ICON_SIZES: [u32; 8] = [16, 20, 24, 32, 40, 48, 64, 256];

/// The icon as a Windows `.ico` file: one PNG-compressed image per size.
pub fn ico(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&size| png(size)).collect();
    let mut out = Vec::new();
    // ICONDIR: reserved, type 1 (icon), count.
    out.extend_from_slice(&[0, 0, 1, 0]);
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (&size, image) in sizes.iter().zip(&images) {
        // ICONDIRENTRY: width and height (0 means 256), colours, reserved,
        // planes, bits per pixel, size, offset.
        let side = if size >= 256 { 0 } else { size as u8 };
        out.extend_from_slice(&[side, side, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&32u16.to_le_bytes());
        out.extend_from_slice(&(image.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += image.len() as u32;
    }
    for image in images {
        out.extend_from_slice(&image);
    }
    out
}

fn png(size: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    let mut encoder = ::png::Encoder::new(&mut bytes, size, size);
    encoder.set_color(::png::ColorType::Rgba);
    encoder.set_depth(::png::BitDepth::Eight);
    let mut writer = encoder.write_header().expect("PNG header");
    writer.write_image_data(&rgba(size)).expect("PNG data");
    writer.finish().expect("PNG end");
    bytes
}

/// The icon as a Windows `HICON` of `size` × `size`.
pub(crate) fn hicon(size: u32) -> Result<HICON> {
    if size == 0 {
        bail!("icon size must be positive");
    }
    let pixels = rgba(size);
    unsafe {
        let info = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: size as i32,
                biHeight: -(size as i32),
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB.0,
                ..Default::default()
            },
            ..Default::default()
        };
        let mut bits = std::ptr::null_mut();
        let color = CreateDIBSection(None, &info, DIB_RGB_COLORS, &mut bits, None, 0)
            .context("CreateDIBSection failed")?;
        let target = std::slice::from_raw_parts_mut(bits.cast::<u8>(), pixels.len());
        for (dst, src) in target
            .as_chunks_mut::<4>()
            .0
            .iter_mut()
            .zip(pixels.as_chunks::<4>().0)
        {
            *dst = [src[2], src[1], src[0], src[3]];
        }
        let mask = CreateBitmap(size as i32, size as i32, 1, 1, None);
        let icon = CreateIconIndirect(&ICONINFO {
            fIcon: true.into(),
            hbmMask: mask,
            hbmColor: color,
            ..Default::default()
        });
        let _ = DeleteObject(HGDIOBJ(color.0));
        let _ = DeleteObject(HGDIOBJ(mask.0));
        icon.context("CreateIconIndirect failed")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn draws_a_dark_square_with_two_coral_corners() {
        let size = 32;
        let px = rgba(size);
        let at =
            |x: u32, y: u32| &px[((y * size + x) * 4) as usize..((y * size + x) * 4 + 4) as usize];
        // Transparent outside the rounded corner, the opaque tile in the middle.
        assert_eq!(at(0, 0)[3], 0);
        assert_eq!(at(16, 16), [0x2A, 0x2E, 0x36, 255]);
        // Coral corners at the top left and bottom right only.
        assert_eq!(at(9, 7), [0xE8, 0x60, 0x3C, 255]);
        assert_eq!(at(22, 24), [0xE8, 0x60, 0x3C, 255]);
        assert_eq!(at(22, 7), [0x2A, 0x2E, 0x36, 255]);
        assert_eq!(at(9, 24), [0x2A, 0x2E, 0x36, 255]);
    }

    #[test]
    fn the_committed_app_icon_matches_the_drawing() {
        // The executable embeds this file (crates/shuttercrab/build.rs). Run
        // with SHUTTERCRAB_WRITE_ICON=1 to regenerate it after changing the
        // drawing.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../shuttercrab/assets/shuttercrab.ico");
        let drawn = ico(&ICON_SIZES);
        if std::env::var_os("SHUTTERCRAB_WRITE_ICON").is_some() {
            std::fs::write(&path, &drawn).unwrap();
        }
        let committed =
            std::fs::read(&path).expect("crates/shuttercrab/assets/shuttercrab.ico exists");
        assert!(
            committed == drawn,
            "the app icon is out of date; see this test"
        );
        // A valid icon directory: type 1, one entry per size, 256 written as 0.
        assert_eq!(&committed[2..6], [1, 0, 8, 0]);
        assert_eq!(committed[6 + 16 * 7], 0);
    }

    #[test]
    fn makes_icons_at_tray_sizes() {
        for size in [16, 20, 24, 32, 48] {
            let icon = hicon(size).unwrap();
            unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyIcon(icon).unwrap() };
        }
        assert!(hicon(0).is_err());
    }
}
