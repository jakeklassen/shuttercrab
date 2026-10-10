//! The icon as a Windows `HICON`, for the tray.

use crate::icon::rgba;
use anyhow::{Context, Result, bail};
use windows::Win32::{
    Graphics::Gdi::{
        BI_RGB, BITMAPINFO, BITMAPINFOHEADER, CreateBitmap, CreateDIBSection, DIB_RGB_COLORS,
        DeleteObject, HGDIOBJ,
    },
    UI::WindowsAndMessaging::{CreateIconIndirect, HICON, ICONINFO},
};

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
    fn makes_icons_at_tray_sizes() {
        for size in [16, 20, 24, 32, 48] {
            let icon = hicon(size).unwrap();
            unsafe { windows::Win32::UI::WindowsAndMessaging::DestroyIcon(icon).unwrap() };
        }
        assert!(hicon(0).is_err());
    }
}
