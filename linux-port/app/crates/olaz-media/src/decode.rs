//! Still-image decoding shared by the UI and the clipboard: JPEG/PNG/WebP/BMP/GIF/JPEG XL → upright RGBA,
//! downscaled so the longer side is at most `max_px`.

use image::{DynamicImage, ImageDecoder, RgbaImage};

pub fn is_jxl(b: &[u8]) -> bool {
    b.starts_with(&[0xFF, 0x0A]) || (b.len() >= 12 && &b[4..8] == b"JXL ")
}

fn decode_jxl(b: &[u8]) -> Option<DynamicImage> {
    let img = jxl_oxide::JxlImage::builder().read(std::io::Cursor::new(b)).ok()?;
    let render = img.render_frame(0).ok()?;
    let fb = render.image_all_channels();
    let (w, h, ch) = (fb.width(), fb.height(), fb.channels());
    let buf = fb.buf();
    let mut rgba = RgbaImage::new(w as u32, h as u32);
    for (i, px) in rgba.pixels_mut().enumerate() {
        let base = i * ch;
        let g = |k: usize| (buf.get(base + k).copied().unwrap_or(1.0).clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        *px = match ch {
            1 => image::Rgba([g(0), g(0), g(0), 255]),
            2 => image::Rgba([g(0), g(0), g(0), g(1)]),
            3 => image::Rgba([g(0), g(1), g(2), 255]),
            _ => image::Rgba([g(0), g(1), g(2), g(3)]),
        };
    }
    Some(DynamicImage::ImageRgba8(rgba))
}

/// Decodes the first frame of `bytes`, applies EXIF orientation and downscales. `None` when undecodable.
pub fn decode_rgba(bytes: &[u8], max_px: u32) -> Option<RgbaImage> {
    let mut img = if is_jxl(bytes) {
        decode_jxl(bytes)?
    } else {
        let reader = image::ImageReader::new(std::io::Cursor::new(bytes)).with_guessed_format().ok()?;
        let mut decoder = reader.into_decoder().ok()?;
        let orientation = decoder.orientation().ok();
        let mut img = DynamicImage::from_decoder(decoder).ok()?;
        if let Some(o) = orientation {
            img.apply_orientation(o);
        }
        img
    };
    if img.width().max(img.height()) > max_px {
        img = img.resize(max_px, max_px, image::imageops::FilterType::Triangle);
    }
    Some(img.into_rgba8())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_png_and_downscales() {
        let mut png = Vec::new();
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(3000, 1000, image::Rgba([10, 20, 30, 255])))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let img = decode_rgba(&png, 1024).unwrap();
        assert_eq!(img.dimensions(), (1024, 341));
        assert!(decode_rgba(b"garbage", 100).is_none());
        assert!(is_jxl(&[0xff, 0x0a, 0xfa]));
        assert!(!is_jxl(&[0xff, 0xd8]));
    }
}
