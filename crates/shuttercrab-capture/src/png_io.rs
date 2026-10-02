//! PNG files: 8-bit sRGB screenshots, and the 16-bit PQ test image.

use anyhow::{Context, Result, bail, ensure};
use std::{fs::File, io::BufWriter, path::Path};

/// Write opaque RGBA8 as an sRGB PNG. The sRGB chunk (plus the gAMA and cHRM
/// fallbacks the encoder adds) tells color-managed viewers what the codes mean.
pub fn write_srgb(path: &Path, width: u32, height: u32, rgba: &[u8]) -> Result<()> {
    ensure!(
        rgba.len() == (width * height * 4) as usize,
        "RGBA buffer has the wrong size"
    );
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    encoder.set_source_srgb(png::SrgbRenderingIntent::Perceptual);
    encoder.set_compression(png::Compression::Fast);
    let mut writer = encoder.write_header()?;
    writer.write_image_data(rgba)?;
    writer.finish()?;
    Ok(())
}

/// Read an 8-bit RGB or RGBA PNG as RGBA8. Color metadata is ignored: the
/// codes are compared as they are.
pub fn read_rgba8(path: &Path) -> Result<(u32, u32, Vec<u8>)> {
    let file = File::open(path).with_context(|| format!("opening {}", path.display()))?;
    let mut reader = png::Decoder::new(std::io::BufReader::new(file)).read_info()?;
    let mut buffer = vec![0; reader.output_buffer_size().context("PNG too large")?];
    let info = reader.next_frame(&mut buffer)?;
    ensure!(
        info.bit_depth == png::BitDepth::Eight,
        "{} is not 8-bit",
        path.display()
    );
    buffer.truncate(info.buffer_size());
    let rgba = match info.color_type {
        png::ColorType::Rgba => buffer,
        png::ColorType::Rgb => buffer
            .as_chunks::<3>()
            .0
            .iter()
            .flat_map(|p| [p[0], p[1], p[2], 255])
            .collect(),
        other => bail!("{} is {other:?}; expected RGB or RGBA", path.display()),
    };
    Ok((info.width, info.height, rgba))
}

/// Write 16-bit RGB tagged as BT.2100 PQ with a cICP chunk (primaries 9 =
/// BT.2020, transfer 16 = PQ, matrix 0 = RGB, full range). Browsers that
/// support HDR PNG render it as HDR.
pub fn write_pq16(path: &Path, width: u32, height: u32, rgb: &[u16]) -> Result<()> {
    ensure!(
        rgb.len() == (width * height * 3) as usize,
        "RGB buffer has the wrong size"
    );
    let file = File::create(path).with_context(|| format!("creating {}", path.display()))?;
    let mut encoder = png::Encoder::new(BufWriter::new(file), width, height);
    encoder.set_color(png::ColorType::Rgb);
    encoder.set_depth(png::BitDepth::Sixteen);
    let mut writer = encoder.write_header()?;
    writer.write_chunk(png::chunk::cICP, &[9, 16, 0, 1])?;
    let bytes: Vec<u8> = rgb.iter().flat_map(|v| v.to_be_bytes()).collect();
    writer.write_image_data(&bytes)?;
    writer.finish()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// A file for the test under the workspace's gitignored `tmp`.
    fn scratch(name: &str) -> PathBuf {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tmp/tests");
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(format!("{name}-{}.png", std::process::id()))
    }

    #[test]
    fn srgb_png_round_trips_with_its_chunk() {
        let path = scratch("srgb");
        let pixels = [255, 255, 255, 255, 247, 247, 247, 255, 0, 128, 255, 255];
        write_srgb(&path, 3, 1, &pixels).unwrap();
        let decoder = png::Decoder::new(std::io::BufReader::new(File::open(&path).unwrap()));
        let reader = decoder.read_info().unwrap();
        assert_eq!(
            reader.info().srgb,
            Some(png::SrgbRenderingIntent::Perceptual)
        );
        drop(reader);
        assert_eq!(read_rgba8(&path).unwrap(), (3, 1, pixels.to_vec()));
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn pq_png_carries_cicp() {
        let path = scratch("pq");
        write_pq16(&path, 2, 1, &[0, 32768, 65535, 1, 2, 3]).unwrap();
        let decoder = png::Decoder::new(std::io::BufReader::new(File::open(&path).unwrap()));
        let reader = decoder.read_info().unwrap();
        let cicp = reader.info().coding_independent_code_points.unwrap();
        assert_eq!(
            (
                cicp.color_primaries,
                cicp.transfer_function,
                cicp.matrix_coefficients
            ),
            (9, 16, 0)
        );
        assert!(cicp.is_video_full_range_image);
        drop(reader);
        std::fs::remove_file(&path).unwrap();
    }
}
