//! The wallpaper, sharp and blurred.
//!
//! Blur is what the reference shells are built around, and it needs something
//! to blur. cusk cleared to a flat colour, and a blurred flat colour is the
//! same flat colour — so the wallpaper is not a companion feature here, it is
//! the thing that makes blur visible at all.
//!
//! # Why this is on the CPU, and why it runs once
//!
//! The usual implementation is a dual-Kawase blur in a fragment shader,
//! ping-ponging between framebuffers every frame. That is the right design
//! when the thing behind a window changes every frame — and the wrong one
//! here, twice over:
//!
//! - **The wallpaper is static.** Re-blurring an unchanging image 60 times a
//!   second is work with no output.
//! - **This runs on llvmpipe.** cusk's own logs report `failed to create dri2
//!   screen`, so EGL is software-rasterised. A multi-pass blur per frame would
//!   be the most expensive thing in the compositor by a wide margin.
//!
//! The honest cost of the choice: what shows through a window is the blurred
//! *wallpaper*, not the blurred contents of whatever window is behind it. Real
//! per-frame blur is a later milestone and needs the shader path; this is the
//! visible nine tenths of the effect for a fraction of the machinery.
//!
//! The upside of doing it in software is that the blur becomes an ordinary
//! pure function over a byte buffer, testable exhaustively without a GPU, a
//! surface, or a running compositor.

use std::path::Path;

/// An RGBA8 image, ready to upload.
#[derive(Debug, Clone, PartialEq)]
pub struct Image {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

impl Image {
    pub fn new(width: u32, height: u32, data: Vec<u8>) -> Self {
        debug_assert_eq!(data.len(), (width * height * 4) as usize);
        Image { data, width, height }
    }

    #[cfg(test)]
    fn index(&self, x: u32, y: u32) -> usize {
        ((y * self.width + x) * 4) as usize
    }

    #[cfg(test)]
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        let i = self.index(x, y);
        [self.data[i], self.data[i + 1], self.data[i + 2], self.data[i + 3]]
    }
}

/// Blur an image by `passes` box blurs of the given `radius`.
///
/// Three box blurs approximate a Gaussian closely enough that the difference
/// is invisible at wallpaper scale, and each is separable — a horizontal pass
/// then a vertical one — which makes the cost linear in the radius rather than
/// quadratic.
pub fn blur(image: &Image, radius: u32, passes: u32) -> Image {
    if radius == 0 || passes == 0 || image.width == 0 || image.height == 0 {
        return image.clone();
    }
    let mut current = image.clone();
    for _ in 0..passes {
        current = box_pass(&current, radius, Axis::Horizontal);
        current = box_pass(&current, radius, Axis::Vertical);
    }
    current
}

#[derive(Clone, Copy)]
enum Axis {
    Horizontal,
    Vertical,
}

/// One separable box blur.
///
/// Uses a running sum rather than re-adding the window at every pixel, so the
/// cost per pixel is constant regardless of radius. Edges clamp to the nearest
/// pixel; treating outside as transparent black instead darkens the borders,
/// which on a wallpaper reads as a vignette nobody asked for.
fn box_pass(src: &Image, radius: u32, axis: Axis) -> Image {
    let (w, h) = (src.width, src.height);
    let mut out = vec![0u8; src.data.len()];
    let window = radius * 2 + 1;

    let (outer, inner) = match axis {
        Axis::Horizontal => (h, w),
        Axis::Vertical => (w, h),
    };

    let at = |x: u32, y: u32| -> usize { ((y * w + x) * 4) as usize };
    let coords = |o: u32, i: u32| -> (u32, u32) {
        match axis {
            Axis::Horizontal => (i, o),
            Axis::Vertical => (o, i),
        }
    };

    for o in 0..outer {
        let sample = |i: i64| -> [u32; 4] {
            let clamped = i.clamp(0, inner as i64 - 1) as u32;
            let (x, y) = coords(o, clamped);
            let p = at(x, y);
            [
                src.data[p] as u32,
                src.data[p + 1] as u32,
                src.data[p + 2] as u32,
                src.data[p + 3] as u32,
            ]
        };

        // Prime the running sum with the window centred on the first pixel.
        let mut sum = [0u32; 4];
        for i in -(radius as i64)..=(radius as i64) {
            let s = sample(i);
            for c in 0..4 {
                sum[c] += s[c];
            }
        }

        for i in 0..inner {
            let (x, y) = coords(o, i);
            let p = at(x, y);
            for c in 0..4 {
                out[p + c] = (sum[c] / window) as u8;
            }
            // Slide: drop the trailing sample, take the leading one.
            let leaving = sample(i as i64 - radius as i64);
            let entering = sample(i as i64 + radius as i64 + 1);
            for c in 0..4 {
                sum[c] = sum[c] - leaving[c] + entering[c];
            }
        }
    }

    Image::new(w, h, out)
}

/// The source rectangle that makes an image cover `target` without distortion.
///
/// Cover rather than fit: letterboxing leaves bars in a colour nobody chose,
/// and stretching is immediately visible on anything with a horizon in it.
pub fn cover_crop(image: (u32, u32), target: (u32, u32)) -> (f64, f64, f64, f64) {
    let (iw, ih) = (image.0 as f64, image.1 as f64);
    let (tw, th) = (target.0 as f64, target.1 as f64);
    if iw <= 0.0 || ih <= 0.0 || tw <= 0.0 || th <= 0.0 {
        return (0.0, 0.0, iw.max(1.0), ih.max(1.0));
    }

    let image_aspect = iw / ih;
    let target_aspect = tw / th;

    if image_aspect > target_aspect {
        // Source is wider: take a full-height slice from the middle.
        let width = ih * target_aspect;
        ((iw - width) / 2.0, 0.0, width, ih)
    } else {
        let height = iw / target_aspect;
        (0.0, (ih - height) / 2.0, iw, height)
    }
}

/// Decode a wallpaper and scale it to cover the output.
///
/// Split from the blur deliberately, on measurement rather than instinct. On a
/// 1920x1080 source in a debug build: decode 179ms, **resize 1278ms** with
/// Lanczos3, blur 160ms. The blur was never the expensive part — so keeping
/// this result and re-blurring it turns a blur-radius change from a 2.1s stall
/// into a fraction of one.
pub fn load_scaled(path: &Path, output: (u32, u32)) -> Result<Image, String> {
    use image::imageops::FilterType;

    let (out_w, out_h) = (output.0.max(1), output.1.max(1));
    let source = image::open(path).map_err(|e| e.to_string())?.to_rgba8();

    let (cx, cy, cw, ch) = cover_crop((source.width(), source.height()), (out_w, out_h));
    let cropped = image::imageops::crop_imm(
        &source,
        cx as u32,
        cy as u32,
        (cw as u32).max(1),
        (ch as u32).max(1),
    )
    .to_image();

    // CatmullRom rather than Lanczos3: around half the cost, and the
    // difference on a photograph downscaled by less than 2x is not visible.
    // Lanczos3 was two thirds of the entire wallpaper pipeline.
    let scaled = image::imageops::resize(&cropped, out_w, out_h, FilterType::CatmullRom);
    Ok(Image::new(out_w, out_h, scaled.into_raw()))
}

/// Blur an already-scaled wallpaper, working at half resolution.
///
/// Blur is a low-pass filter, so the detail dropped by halving is detail the
/// blur would have destroyed anyway; the result is indistinguishable and costs
/// a quarter of the pixels. Same reasoning as the downsample step of a
/// dual-Kawase shader, done on the CPU.
///
/// Returns an image the same size as its input, so one window rectangle
/// indexes the sharp and blurred textures identically. Two scales would mean
/// two sets of coordinate maths, and the second would be the one with the bug.
pub fn blurred_from(sharp: &Image, radius: u32, passes: u32) -> Image {
    use image::imageops::FilterType;

    let (w, h) = (sharp.width, sharp.height);
    let (half_w, half_h) = ((w / 2).max(1), (h / 2).max(1));

    let Some(full) = image::RgbaImage::from_raw(w, h, sharp.data.clone()) else {
        return sharp.clone();
    };
    let small = image::imageops::resize(&full, half_w, half_h, FilterType::Triangle);

    // The radius halves with the resolution, or the blur comes out twice as
    // wide as the number the user set.
    let blurred = blur(
        &Image::new(half_w, half_h, small.into_raw()),
        (radius / 2).max(1),
        passes,
    );

    let Some(small) = image::RgbaImage::from_raw(half_w, half_h, blurred.data) else {
        return sharp.clone();
    };
    let restored = image::imageops::resize(&small, w, h, FilterType::Triangle);
    Image::new(w, h, restored.into_raw())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(w: u32, h: u32, colour: [u8; 4]) -> Image {
        Image::new(w, h, colour.iter().copied().cycle().take((w * h * 4) as usize).collect())
    }

    /// The property that catches almost every blur bug: a uniform image must
    /// survive unchanged. Off-by-one windows, bad edge handling and integer
    /// truncation all show up here as a shift in value.
    #[test]
    fn a_uniform_image_is_unchanged_by_blur() {
        let image = solid(16, 16, [80, 120, 200, 255]);
        assert_eq!(blur(&image, 4, 3), image);
    }

    /// Edges clamp rather than fading to transparent black. Blurring a solid
    /// image and finding the corners darker is the vignette this avoids.
    #[test]
    fn edges_do_not_darken() {
        let image = solid(12, 12, [200, 200, 200, 255]);
        let blurred = blur(&image, 3, 2);
        for (x, y) in [(0, 0), (11, 0), (0, 11), (11, 11), (5, 0), (0, 5)] {
            assert_eq!(
                blurred.pixel(x, y),
                [200, 200, 200, 255],
                "corner/edge ({x},{y}) darkened"
            );
        }
    }

    #[test]
    fn blur_preserves_dimensions() {
        let image = solid(23, 7, [10, 20, 30, 255]);
        let blurred = blur(&image, 5, 3);
        assert_eq!((blurred.width, blurred.height), (23, 7));
        assert_eq!(blurred.data.len(), image.data.len());
    }

    /// A single bright pixel must spread outwards, and must not stay put.
    #[test]
    fn an_impulse_spreads() {
        let mut image = solid(21, 21, [0, 0, 0, 255]);
        let centre = image.index(10, 10);
        image.data[centre] = 255;

        let blurred = blur(&image, 3, 1);
        assert!(blurred.pixel(10, 10)[0] < 255, "the peak must fall");
        assert!(blurred.pixel(11, 10)[0] > 0, "light must reach the neighbour");
        assert!(blurred.pixel(10, 11)[0] > 0, "and vertically too");
        assert_eq!(blurred.pixel(0, 0)[0], 0, "but not across the whole image");
    }

    /// More passes must blur more, not less. A pass that reads its own output
    /// incorrectly can converge or oscillate instead.
    #[test]
    fn more_passes_blur_further() {
        let mut image = solid(41, 41, [0, 0, 0, 255]);
        let centre = image.index(20, 20);
        image.data[centre] = 255;

        let once = blur(&image, 3, 1);
        let thrice = blur(&image, 3, 3);
        assert!(
            thrice.pixel(20, 20)[0] <= once.pixel(20, 20)[0],
            "three passes must not leave a sharper peak than one"
        );
        assert!(
            thrice.pixel(26, 20)[0] >= once.pixel(26, 20)[0],
            "three passes must reach further out"
        );
    }

    #[test]
    fn a_zero_radius_is_a_no_op() {
        let image = solid(8, 8, [1, 2, 3, 4]);
        assert_eq!(blur(&image, 0, 3), image);
        assert_eq!(blur(&image, 5, 0), image);
    }

    /// A radius wider than the image must not panic or read out of bounds.
    #[test]
    fn a_radius_larger_than_the_image_is_safe() {
        let image = solid(4, 4, [100, 100, 100, 255]);
        assert_eq!(blur(&image, 50, 2), image);
    }

    #[test]
    fn cover_crop_takes_the_middle_of_a_wide_image() {
        assert_eq!(cover_crop((4000, 1000), (1000, 1000)), (1500.0, 0.0, 1000.0, 1000.0));
    }

    #[test]
    fn cover_crop_takes_the_middle_of_a_tall_image() {
        assert_eq!(cover_crop((1000, 4000), (1000, 1000)), (0.0, 1500.0, 1000.0, 1000.0));
    }

    /// A matching aspect ratio must use the whole image, with no crop at all —
    /// otherwise every wallpaper loses a sliver for no reason.
    #[test]
    fn cover_crop_of_a_matching_aspect_is_the_whole_image() {
        assert_eq!(cover_crop((1920, 1080), (3840, 2160)), (0.0, 0.0, 1920.0, 1080.0));
    }

    #[test]
    fn cover_crop_survives_degenerate_sizes() {
        let (_, _, w, h) = cover_crop((0, 0), (100, 100));
        assert!(w > 0.0 && h > 0.0);
    }

    /// Both images must be exactly output-sized, because the renderer uses a
    /// window's rectangle as a source crop into either without rescaling.
    #[test]
    fn the_sharp_and_blurred_images_are_the_same_size() {
        let dir = std::env::temp_dir().join("cusk-wallpaper-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("w.png");
        // A deliberately wrong aspect ratio, so the cover crop has work to do,
        // and high-frequency detail so the blur has something to remove.
        let source = image::RgbaImage::from_fn(400, 100, |x, y| {
            let v = if (x / 3 + y / 3) % 2 == 0 { 255 } else { 20 };
            image::Rgba([v, 60, 120, 255])
        });
        source.save(&path).unwrap();

        let sharp = load_scaled(&path, (320, 240)).unwrap();
        assert_eq!((sharp.width, sharp.height), (320, 240));

        let blurred = blurred_from(&sharp, 16, 3);
        assert_eq!((blurred.width, blurred.height), (320, 240));
        assert_eq!(sharp.data.len(), blurred.data.len());
        assert_ne!(blurred.data, sharp.data, "a detailed image must actually change");
    }

    #[test]
    fn a_missing_wallpaper_is_an_error_not_a_panic() {
        let missing = Path::new("/nonexistent/cusk/wallpaper.png");
        assert!(load_scaled(missing, (100, 100)).is_err());
    }
}
