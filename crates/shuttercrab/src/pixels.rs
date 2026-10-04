//! Screenshots' pixels between the forms Shuttercrab keeps them in: straight
//! RGBA8 from the capture, and BGRA8 images that GPUI draws.

use gpui_kit::RenderImage;
use std::sync::Arc;

/// A `width` × `height` straight-alpha RGBA8 image as one GPUI can draw.
pub fn render_image(rgba: &[u8], width: u32, height: u32) -> Arc<RenderImage> {
    let mut bgra = rgba.to_vec();
    // GPUI draws BGRA.
    for px in bgra.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    bgra_image(bgra, width, height)
}

/// A `width` × `height` straight-alpha BGRA8 image, already in GPUI's
/// channel order, as one it can draw.
pub fn bgra_image(bgra: Vec<u8>, width: u32, height: u32) -> Arc<RenderImage> {
    let buffer =
        image::RgbaImage::from_raw(width, height, bgra).expect("the buffer matches its size");
    Arc::new(RenderImage::new([image::Frame::new(buffer)]))
}

/// The straight-alpha RGBA8 pixels of an image from [`render_image`].
pub fn rgba(image: &RenderImage) -> Vec<u8> {
    let mut rgba = image.as_bytes(0).unwrap_or_default().to_vec();
    for px in rgba.as_chunks_mut::<4>().0 {
        px.swap(0, 2);
    }
    rgba
}

/// A PNG file's pixels as straight-alpha RGBA8, with the width and height.
pub fn decode_png(png: &[u8]) -> anyhow::Result<(Vec<u8>, u32, u32)> {
    let image = image::load_from_memory_with_format(png, image::ImageFormat::Png)?.into_rgba8();
    let (width, height) = image.dimensions();
    Ok((image.into_raw(), width, height))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pixels_come_back_as_they_went_in() {
        let rgba = [10, 20, 30, 255, 40, 50, 60, 128];
        let image = render_image(&rgba, 2, 1);
        assert_eq!(
            image.as_bytes(0).unwrap(),
            &[30, 20, 10, 255, 60, 50, 40, 128]
        );
        assert_eq!(super::rgba(&image), rgba);
    }
}
