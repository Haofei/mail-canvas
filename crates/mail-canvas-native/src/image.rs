use std::io::Cursor;

use anyhow::{Context as _, Result, bail};
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use mail_canvas_core::{ImageData, repair_png_chunk_crcs};

pub(crate) fn decode_image_bytes(
    bytes: &[u8],
    policy: &mail_canvas_core::ResourcePolicy,
) -> Result<ImageData> {
    ensure_resource_size(bytes.len(), policy.max_resource_bytes)?;
    if looks_like_svg(bytes) {
        return decode_svg_bytes(bytes, policy);
    }
    match decode_image_bytes_strict(bytes, policy) {
        Ok(image) => Ok(image),
        Err(error) => {
            let Some(repaired) = repair_png_chunk_crcs(bytes) else {
                return Err(error);
            };
            decode_image_bytes_strict(&repaired, policy)
                .with_context(|| format!("failed to decode image after PNG CRC repair: {error}"))
        }
    }
}

fn decode_image_bytes_strict(
    bytes: &[u8],
    policy: &mail_canvas_core::ResourcePolicy,
) -> Result<ImageData> {
    let max_side = u32::try_from(policy.max_decoded_pixels.min(u64::from(u32::MAX)))
        .expect("bounded decoded pixel limit");
    let mut reader = ImageReader::new(Cursor::new(bytes));
    let mut limits = Limits::default();
    limits.max_image_width = Some(max_side);
    limits.max_image_height = Some(max_side);
    limits.max_alloc = Some(policy.max_decoded_pixels.saturating_mul(4));
    reader.limits(limits);
    let mut decoder = reader
        .with_guessed_format()
        .context("failed to guess image format")?
        .into_decoder()
        .context("failed to create image decoder")?;
    let orientation = decoder
        .orientation()
        .context("failed to read image orientation")?;
    let mut image = DynamicImage::from_decoder(decoder).context("failed to decode image")?;
    image.apply_orientation(orientation);
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > policy.max_decoded_pixels {
        bail!(
            "decoded image is too large: {pixels} pixels > {} pixels",
            policy.max_decoded_pixels
        );
    }
    Ok(ImageData {
        width,
        height,
        rgba: rgba.into_raw().into(),
    })
}

fn decode_svg_bytes(bytes: &[u8], policy: &mail_canvas_core::ResourcePolicy) -> Result<ImageData> {
    let tree = resvg::usvg::Tree::from_data(bytes, &resvg::usvg::Options::default())
        .context("failed to parse SVG")?;
    let size = tree.size().to_int_size();
    let pixels = u64::from(size.width()).saturating_mul(u64::from(size.height()));
    if pixels > policy.max_decoded_pixels {
        bail!(
            "decoded SVG is too large: {pixels} pixels > {} pixels",
            policy.max_decoded_pixels
        );
    }
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size.width(), size.height())
        .context("failed to allocate SVG pixmap")?;
    resvg::render(
        &tree,
        resvg::tiny_skia::Transform::identity(),
        &mut pixmap.as_mut(),
    );
    let mut rgba = pixmap.take();
    unpremultiply_rgba(&mut rgba);
    Ok(ImageData {
        width: size.width(),
        height: size.height(),
        rgba: rgba.into(),
    })
}

fn looks_like_svg(bytes: &[u8]) -> bool {
    let text = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    trimmed.starts_with("<svg") || (trimmed.starts_with("<?xml") && trimmed.contains("<svg"))
}

fn unpremultiply_rgba(rgba: &mut [u8]) {
    for pixel in rgba.chunks_exact_mut(4) {
        let alpha = u16::from(pixel[3]);
        if alpha == 0 {
            pixel[0] = 0;
            pixel[1] = 0;
            pixel[2] = 0;
        } else if alpha < 255 {
            pixel[0] = ((u16::from(pixel[0]) * 255 + alpha / 2) / alpha).min(255) as u8;
            pixel[1] = ((u16::from(pixel[1]) * 255 + alpha / 2) / alpha).min(255) as u8;
            pixel[2] = ((u16::from(pixel[2]) * 255 + alpha / 2) / alpha).min(255) as u8;
        }
    }
}

fn ensure_resource_size(len: usize, max_len: usize) -> Result<()> {
    if len > max_len {
        bail!("resource is too large: {len} bytes > {max_len} bytes");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use image::{ColorType, codecs::jpeg::JpegEncoder};

    use super::*;

    fn test_policy() -> mail_canvas_core::ResourcePolicy {
        mail_canvas_core::ResourcePolicy {
            allow_remote: false,
            https_only: true,
            deny_private_networks: true,
            timeout: Duration::from_secs(1),
            max_resource_bytes: 1024 * 1024,
            max_total_resource_bytes: 2 * 1024 * 1024,
            max_decoded_pixels: 1024,
            max_resource_count: 8,
        }
    }

    #[test]
    fn decode_applies_exif_orientation_like_blink() {
        let mut jpeg = Vec::new();
        JpegEncoder::new(&mut jpeg)
            .encode(&[255, 0, 0, 0, 255, 0], 1, 2, ColorType::Rgb8.into())
            .expect("encode jpeg");
        let oriented = jpeg_with_exif_orientation(jpeg, 6);

        let image = decode_image_bytes(&oriented, &test_policy()).expect("decode");

        assert_eq!((image.width, image.height), (2, 1));
    }

    #[test]
    fn decode_repairs_invalid_png_chunk_crc_like_browsers() {
        let png = [
            0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
            0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
            0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x44, 0x41, 0x54, 0x08,
            0x1d, 0x63, 0xf8, 0xff, 0xff, 0xff, 0x7f, 0x00, 0x09, 0xfb, 0x03, 0xfd, 0x2a, 0x86,
            0xe3, 0x8a, 0x00, 0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
        ];

        let image = decode_image_bytes(&png, &test_policy()).expect("decode repaired png");

        assert_eq!((image.width, image.height), (1, 1));
        assert_eq!(image.rgba.as_ref(), [255, 255, 255, 255]);
    }

    #[test]
    fn decode_rasterizes_svg_images() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="2" height="1"><rect width="2" height="1" fill="#ff0000"/></svg>"##;

        let image = decode_image_bytes(svg, &test_policy()).expect("decode svg");

        assert_eq!((image.width, image.height), (2, 1));
        assert_eq!(&image.rgba.as_ref()[0..4], &[255, 0, 0, 255]);
    }

    fn jpeg_with_exif_orientation(jpeg: Vec<u8>, orientation: u16) -> Vec<u8> {
        assert_eq!(&jpeg[0..2], &[0xff, 0xd8]);
        let mut exif = Vec::new();
        exif.extend_from_slice(b"Exif\0\0");
        exif.extend_from_slice(b"MM");
        exif.extend_from_slice(&42u16.to_be_bytes());
        exif.extend_from_slice(&8u32.to_be_bytes());
        exif.extend_from_slice(&1u16.to_be_bytes());
        exif.extend_from_slice(&0x0112u16.to_be_bytes());
        exif.extend_from_slice(&3u16.to_be_bytes());
        exif.extend_from_slice(&1u32.to_be_bytes());
        exif.extend_from_slice(&orientation.to_be_bytes());
        exif.extend_from_slice(&0u16.to_be_bytes());
        exif.extend_from_slice(&0u32.to_be_bytes());

        let segment_len = u16::try_from(exif.len() + 2).expect("segment length");
        let mut out = Vec::new();
        out.extend_from_slice(&jpeg[0..2]);
        out.extend_from_slice(&[0xff, 0xe1]);
        out.extend_from_slice(&segment_len.to_be_bytes());
        out.extend_from_slice(&exif);
        out.extend_from_slice(&jpeg[2..]);
        out
    }
}
