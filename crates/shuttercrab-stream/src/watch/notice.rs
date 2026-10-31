//! Telling the viewer things: a notice in place of the pictures, a badge
//! over them, and a message box for a viewer with no window yet.
//!
//! The notice is drawn two ways that look alike on purpose: painted on the
//! window with GDI until there are pictures, because before the first one
//! there is no swap chain to draw into, then drawn into the frames with
//! Direct2D.

use anyhow::Result;
use std::{
    cell::RefCell,
    sync::{Arc, Mutex},
    time::Instant,
};
use windows::{
    Win32::{
        Foundation::{COLORREF, HWND, RECT},
        Graphics::{
            Direct2D::{
                Common::{D2D_RECT_F, D2D1_ALPHA_MODE_IGNORE, D2D1_COLOR_F, D2D1_PIXEL_FORMAT},
                D2D1_DRAW_TEXT_OPTIONS_NONE, D2D1_FACTORY_TYPE_SINGLE_THREADED,
                D2D1_RENDER_TARGET_PROPERTIES, D2D1_ROUNDED_RECT, D2D1CreateFactory, ID2D1Factory,
            },
            Direct3D11::{D3D11_TEXTURE2D_DESC, ID3D11Texture2D},
            DirectWrite::{
                DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_WEIGHT_NORMAL, DWRITE_MEASURING_MODE_NATURAL,
                DWRITE_PARAGRAPH_ALIGNMENT, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                DWRITE_PARAGRAPH_ALIGNMENT_FAR, DWRITE_PARAGRAPH_ALIGNMENT_NEAR,
                DWRITE_TEXT_ALIGNMENT_CENTER, DWriteCreateFactory, IDWriteFactory,
                IDWriteTextFormat,
            },
            Dxgi::{Common::DXGI_FORMAT_B8G8R8A8_UNORM, IDXGISurface},
            Gdi::{
                BLACK_BRUSH, BeginPaint, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW,
                DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_BOTTOM, DT_CENTER, DT_SINGLELINE, DT_TOP,
                DT_WORDBREAK, DeleteObject, DrawTextW, EndPaint, FW_NORMAL, FillRect,
                GetStockObject, HBRUSH, HDC, InvalidateRect, OUT_DEFAULT_PRECIS, PAINTSTRUCT,
                SelectObject, SetBkMode, SetTextColor, TRANSPARENT,
            },
        },
        UI::{
            HiDpi::GetDpiForWindow,
            WindowsAndMessaging::{GetClientRect, MB_ICONINFORMATION, MB_OK, MessageBoxW},
        },
    },
    core::{HSTRING, Interface, w},
};

thread_local! {
    /// What the window says until it has pictures: a headline and a line
    /// below it.
    pub(super) static NOTICE: RefCell<Option<(String, String)>> = const { RefCell::new(None) };
}

/// Have the window say `headline`, and `detail` below it, until it has
/// pictures.
pub(super) fn show_notice(hwnd: HWND, headline: &str, detail: &str) {
    NOTICE.with_borrow_mut(|notice| *notice = Some((headline.into(), detail.into())));
    let _ = unsafe { InvalidateRect(Some(hwnd), None, true) };
}

/// Paint the notice, if there is one: white on black, in the middle.
pub(super) fn paint_notice(hwnd: HWND) -> bool {
    NOTICE.with_borrow(|notice| {
        let Some((headline, detail)) = notice else {
            return false;
        };
        unsafe {
            let mut paint = PAINTSTRUCT::default();
            let dc = BeginPaint(hwnd, &mut paint);
            let mut area = RECT::default();
            let _ = GetClientRect(hwnd, &mut area);
            FillRect(dc, &area, HBRUSH(GetStockObject(BLACK_BRUSH).0));
            SetBkMode(dc, TRANSPARENT);
            let px = |points: i32| points * GetDpiForWindow(hwnd) as i32 / 96;
            let middle = (area.top + area.bottom) / 2;
            let mut above = RECT {
                bottom: middle,
                ..area
            };
            let mut below = RECT {
                left: area.left + px(24),
                top: middle + px(12),
                right: area.right - px(24),
                ..area
            };
            draw_text(
                dc,
                headline,
                (px(28), 0x00F0_F0F0),
                &mut above,
                DT_CENTER | DT_BOTTOM | DT_SINGLELINE,
            );
            draw_text(
                dc,
                detail,
                (px(16), 0x00A0_A0A0),
                &mut below,
                DT_CENTER | DT_TOP | DT_WORDBREAK,
            );
            let _ = EndPaint(hwnd, &paint);
        }
        true
    })
}

/// Draw `text` in Segoe UI `height` pixels high, in `color` (0x00BBGGRR),
/// in `area`.
unsafe fn draw_text(
    dc: HDC,
    text: &str,
    (height, color): (i32, u32),
    area: &mut RECT,
    format: DRAW_TEXT_FORMAT,
) {
    unsafe {
        let font = CreateFontW(
            -height,
            0,
            0,
            0,
            FW_NORMAL.0 as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET,
            OUT_DEFAULT_PRECIS,
            CLIP_DEFAULT_PRECIS,
            CLEARTYPE_QUALITY,
            0,
            w!("Segoe UI"),
        );
        let old = SelectObject(dc, font.into());
        SetTextColor(dc, COLORREF(color));
        let mut text: Vec<u16> = text.encode_utf16().collect();
        DrawTextW(dc, &mut text, area, format);
        SelectObject(dc, old);
        let _ = DeleteObject(font.into());
    }
}

/// Tell the user `text` in a message box titled `title`, for a viewer with
/// no window yet to say it in.
pub fn tell(title: &str, text: &str) {
    unsafe {
        MessageBoxW(
            None,
            &HSTRING::from(text),
            &HSTRING::from(title),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

/// What the window has the drawing thread draw over the pictures.
#[derive(Clone, Debug, Default)]
pub(super) struct Overlay {
    /// A notice in place of the pictures: a headline and a line below it.
    pub(super) notice: Option<(String, String)>,
    /// A label over the pictures' top corner, and until when.
    pub(super) badge: Option<(String, Instant)>,
}

/// The overlay, shared: set by the window, drawn by the drawing thread.
pub(super) type Over = Arc<Mutex<Overlay>>;

/// Draws a notice into a frame: white and grey on black, in the middle,
/// sized to the frame, which the window stretches as it does the pictures.
pub(super) struct Notices {
    d2d: ID2D1Factory,
    write: IDWriteFactory,
}

impl Notices {
    pub(super) fn new() -> Result<Self> {
        unsafe {
            Ok(Self {
                d2d: D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?,
                write: DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?,
            })
        }
    }

    /// Draw `headline` and `detail` into `frame`.
    pub(super) fn draw(
        &self,
        frame: &ID3D11Texture2D,
        (headline, detail): &(String, String),
    ) -> Result<()> {
        let surface: IDXGISurface = frame.cast()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { frame.GetDesc(&mut desc) };
        let (width, height) = (desc.Width as f32, desc.Height as f32);
        let color = |grey: f32| D2D1_COLOR_F {
            r: grey,
            g: grey,
            b: grey,
            a: 1.0,
        };
        unsafe {
            let target = self.d2d.CreateDxgiSurfaceRenderTarget(
                &surface,
                &D2D1_RENDER_TARGET_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_IGNORE,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    ..Default::default()
                },
            )?;
            let (white, grey) = (
                target.CreateSolidColorBrush(&color(0.94), None)?,
                target.CreateSolidColorBrush(&color(0.63), None)?,
            );
            // As the window's own notice is, for a frame shown at about
            // 1080 pixels high.
            let scale = height / 1080.0;
            let middle = height / 2.0;
            let margin = width * 0.1;
            target.BeginDraw();
            target.Clear(Some(&color(0.0)));
            target.DrawText(
                &headline.encode_utf16().collect::<Vec<_>>(),
                &self.format(40.0 * scale, DWRITE_PARAGRAPH_ALIGNMENT_FAR)?,
                &D2D_RECT_F {
                    left: margin,
                    top: 0.0,
                    right: width - margin,
                    bottom: middle,
                },
                &white,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            target.DrawText(
                &detail.encode_utf16().collect::<Vec<_>>(),
                &self.format(22.0 * scale, DWRITE_PARAGRAPH_ALIGNMENT_NEAR)?,
                &D2D_RECT_F {
                    left: margin,
                    top: middle + 16.0 * scale,
                    right: width - margin,
                    bottom: height,
                },
                &grey,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            target.EndDraw(None, None)?;
        }
        Ok(())
    }

    /// Draw `label` over `frame`'s top right corner, white on a rounded
    /// dark box, the picture showing around it.
    pub(super) fn badge(&self, frame: &ID3D11Texture2D, label: &str) -> Result<()> {
        let surface: IDXGISurface = frame.cast()?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { frame.GetDesc(&mut desc) };
        let (width, height) = (desc.Width as f32, desc.Height as f32);
        // As for the notice: sized for a frame shown about 1080 pixels high.
        let scale = height / 1080.0;
        let (box_width, box_height, margin) = (260.0 * scale, 64.0 * scale, 28.0 * scale);
        let area = D2D_RECT_F {
            left: width - margin - box_width,
            top: margin,
            right: width - margin,
            bottom: margin + box_height,
        };
        unsafe {
            let target = self.d2d.CreateDxgiSurfaceRenderTarget(
                &surface,
                &D2D1_RENDER_TARGET_PROPERTIES {
                    pixelFormat: D2D1_PIXEL_FORMAT {
                        format: DXGI_FORMAT_B8G8R8A8_UNORM,
                        alphaMode: D2D1_ALPHA_MODE_IGNORE,
                    },
                    dpiX: 96.0,
                    dpiY: 96.0,
                    ..Default::default()
                },
            )?;
            let shade = target.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.08,
                    g: 0.08,
                    b: 0.08,
                    a: 0.85,
                },
                None,
            )?;
            let white = target.CreateSolidColorBrush(
                &D2D1_COLOR_F {
                    r: 0.94,
                    g: 0.94,
                    b: 0.94,
                    a: 1.0,
                },
                None,
            )?;
            let format = self.format(28.0 * scale, DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            target.BeginDraw();
            target.FillRoundedRectangle(
                &D2D1_ROUNDED_RECT {
                    rect: area,
                    radiusX: 12.0 * scale,
                    radiusY: 12.0 * scale,
                },
                &shade,
            );
            target.DrawText(
                &label.encode_utf16().collect::<Vec<_>>(),
                &format,
                &area,
                &white,
                D2D1_DRAW_TEXT_OPTIONS_NONE,
                DWRITE_MEASURING_MODE_NATURAL,
            );
            target.EndDraw(None, None)?;
        }
        Ok(())
    }

    /// Segoe UI `size` pixels high, centred, against the top, the middle or
    /// the bottom of its box.
    fn format(&self, size: f32, against: DWRITE_PARAGRAPH_ALIGNMENT) -> Result<IDWriteTextFormat> {
        unsafe {
            let format = self.write.CreateTextFormat(
                w!("Segoe UI"),
                None,
                DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!(""),
            )?;
            format.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_CENTER)?;
            format.SetParagraphAlignment(against)?;
            Ok(format)
        }
    }
}
