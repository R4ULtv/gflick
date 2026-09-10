// Shared by the photo-baking build script and its test; never called at runtime.
use image::{Rgba, Rgba32FImage, RgbaImage, imageops::FilterType};

/// Filter premultiplied colors so transparent pixels cannot introduce an edge
/// halo, then restore straight alpha for GPUI. The source asset stays untouched.
pub fn downsample(mut source: Rgba32FImage, width: u32, height: u32) -> RgbaImage {
    for pixel in source.pixels_mut() {
        // Keep headroom for Lanczos ringing: image::resize clamps float
        // channels to 1, which would otherwise clip alpha before RGB.
        pixel[3] *= 0.5;
        for channel in 0..3 {
            pixel[channel] *= pixel[3];
        }
    }
    let scaled = image::imageops::resize(&source, width, height, FilterType::Lanczos3);
    RgbaImage::from_fn(width, height, |x, y| {
        let pixel = scaled.get_pixel(x, y);
        let alpha = (pixel[3] * 2.0).clamp(0.0, 1.0);
        if alpha < 1.0 / 255.0 {
            return Rgba([0; 4]);
        }
        Rgba([
            ((pixel[0] / pixel[3]).clamp(0.0, 1.0) * 255.0).round() as u8,
            ((pixel[1] / pixel[3]).clamp(0.0, 1.0) * 255.0).round() as u8,
            ((pixel[2] / pixel[3]).clamp(0.0, 1.0) * 255.0).round() as u8,
            (alpha * 255.0).round() as u8,
        ])
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn transparent_colors_do_not_bleed_into_mouse_edges() {
        let source = Rgba32FImage::from_fn(16, 16, |x, _| {
            if x < 8 {
                Rgba([0.2, 0.2, 0.2, 1.0])
            } else {
                Rgba([1.0, 0.0, 1.0, 0.0])
            }
        });
        let scaled = downsample(source, 5, 5);
        assert!(scaled.pixels().any(|p| p[3] > 0 && p[3] < 255));
        for p in scaled.pixels().filter(|p| p[3] > 0) {
            assert_eq!(p[0], p[1]);
            assert_eq!(p[1], p[2]);
            assert!((i16::from(p[0]) - 51).abs() <= 1);
        }
    }
}
