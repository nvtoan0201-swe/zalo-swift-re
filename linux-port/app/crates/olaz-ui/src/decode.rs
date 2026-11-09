//! Image decoding off the main thread: JPEG/PNG/WebP/BMP/JPEG XL → BGRA `RenderImage`, downscaled to
//! the pixel bucket and turned upright by EXIF orientation; animated GIF/WebP and SVG stay encoded so GPUI
//! animates/rasterizes them.

use std::sync::Arc;

use gpui::{Image, ImageFormat, ImageSource, RenderImage};

pub const MAX_PIXELS: u32 = 2048;

fn is_animated(b: &[u8]) -> bool {
    if b.starts_with(b"GIF8") {
        // more than one image descriptor → animated
        return b.windows(2).filter(|w| w[0] == 0x00 && w[1] == 0x2C).count() > 1;
    }
    b.len() > 30 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" && b[12..16] == *b"VP8X" && (b[20] & 0x02) != 0
}

/// A decoded picture (Send, unlike `ImageSource`).
pub enum Decoded {
    Encoded(Arc<Image>),
    Render(Arc<RenderImage>),
}

impl Decoded {
    pub fn source(self) -> ImageSource {
        match self {
            Decoded::Encoded(i) => ImageSource::Image(i),
            Decoded::Render(r) => ImageSource::Render(r),
        }
    }
}

/// Decodes `bytes` into something `img()` can draw. `None` when undecodable.
pub fn decode(bytes: &[u8], max_px: u32) -> Option<Decoded> {
    match crate::widgets::detect_format(bytes) {
        Some(ImageFormat::Svg) => return Some(Decoded::Encoded(Arc::new(Image::from_bytes(ImageFormat::Svg, bytes.to_vec())))),
        Some(f @ (ImageFormat::Gif | ImageFormat::Webp)) if is_animated(bytes) => {
            return Some(Decoded::Encoded(Arc::new(Image::from_bytes(f, bytes.to_vec()))));
        }
        _ => {}
    }
    let mut rgba = olaz_media::decode::decode_rgba(bytes, max_px)?;
    for px in rgba.pixels_mut() {
        px.0.swap(0, 2); // RGBA → BGRA
    }
    let frame = image::Frame::new(rgba);
    Some(Decoded::Render(Arc::new(RenderImage::new(smallvec::smallvec![frame]))))
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{DynamicImage, RgbaImage};

    #[test]
    fn decodes_png_and_downscales() {
        let mut png = Vec::new();
        DynamicImage::ImageRgba8(RgbaImage::from_pixel(3000, 1000, image::Rgba([10, 20, 30, 255])))
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        let Some(Decoded::Render(r)) = decode(&png, 1024) else { panic!() };
        let size = r.size(0);
        assert_eq!((size.width.0, size.height.0), (1024, 341));
        assert!(decode(b"garbage", 100).is_none());
    }
}
