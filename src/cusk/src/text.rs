//! Drawing text.
//!
//! The gate on most of what a panel wants — a window title, a clock, workspace
//! names — and on anything the launcher might show beyond its own list. cusk
//! could not draw a glyph until now, which is why milestone 15's indicator is
//! rectangles.
//!
//! `fontdue` rather than a shaping engine. It rasterises a glyph to a coverage
//! bitmap and reports its metrics, which is exactly the job here and no more.
//! What it does not do is worth stating rather than discovering: no shaping, so
//! no ligatures, no cursive joining, and no reordering — Latin, Greek and
//! Cyrillic come out right, Arabic and Devanagari do not. A window title in a
//! script that needs shaping will be visibly wrong rather than absent, which is
//! the honest failure but still a failure. `cosmic-text` is the upgrade path.
//!
//! Nothing is bundled. A font is found on the system, because shipping one
//! means a licence decision and a few hundred kilobytes to say what
//! `/usr/share/fonts` already says.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::wallpaper::Image;

/// Fonts to look for, in order, when none is configured.
///
/// Ordinary sans faces that a Linux system is overwhelmingly likely to have.
/// The list is short on purpose: a long one is a slower miss, and the answer to
/// "my font is not here" is `appearance.font`, not a longer list.
const CANDIDATES: &[&str] = &[
    "/usr/share/fonts/noto/NotoSans-Regular.ttf",
    "/usr/share/fonts/liberation-fonts/LiberationSans-Regular.ttf",
    "/usr/share/fonts/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/truetype/dejavu/DejaVuSans.ttf",
    "/usr/share/fonts/TTF/DejaVuSans.ttf",
    "/usr/share/fonts/urw-fonts/NimbusSans-Regular.ttf",
    "/usr/share/fonts/liberation/LiberationSans-Regular.ttf",
];

/// The first usable font, or `None` if the system has none of them.
pub fn find_font(configured: &str) -> Option<PathBuf> {
    let configured = configured.trim();
    if !configured.is_empty() {
        let path = PathBuf::from(configured);
        // A configured font that does not exist is not silently replaced. The
        // user asked for that file; falling back would leave them staring at
        // the wrong typeface with nothing said about it.
        return path.is_file().then_some(path);
    }
    CANDIDATES
        .iter()
        .map(Path::new)
        .find(|path| path.is_file())
        .map(Path::to_path_buf)
}

/// A loaded face, with a cache of strings already rasterised.
pub struct Face {
    font: fontdue::Font,
    /// Rasterising a string costs a pass over every glyph. A window title
    /// changes rarely and is drawn every frame, so without this the panel
    /// would re-rasterise the same string sixty times a second.
    cache: HashMap<(String, u32), Option<Image>>,
}

impl Face {
    pub fn load(path: &Path) -> Option<Self> {
        let data = std::fs::read(path).ok()?;
        let font = fontdue::Font::from_bytes(data, fontdue::FontSettings::default()).ok()?;
        Some(Face { font, cache: HashMap::new() })
    }

    /// Height of one line at this size, in pixels.
    pub fn line_height(&self, px: f32) -> i32 {
        match self.font.horizontal_line_metrics(px) {
            // Descent is negative, so this is ascent + |descent|.
            Some(metrics) => (metrics.ascent - metrics.descent).ceil() as i32,
            None => px.ceil() as i32,
        }
    }

    /// Width of a string, in pixels, without rasterising it.
    ///
    /// Advances are summed as floats and rounded once at the end. Rounding
    /// each glyph instead accumulates up to half a pixel per character, which
    /// on a long title is a visible drift between the measured width and the
    /// drawn one — and the measurement is what truncation and right-alignment
    /// depend on.
    pub fn measure(&self, text: &str, px: f32) -> i32 {
        let mut width = 0.0f32;
        for character in text.chars() {
            width += self.font.metrics(character, px).advance_width;
        }
        width.ceil() as i32
    }

    /// Shorten a string with an ellipsis so it fits within `max_width`.
    ///
    /// Returns the text unchanged when it already fits. Truncating by
    /// character count instead of measured width would cut a wide title too
    /// early and a narrow one too late — proportional fonts have no character
    /// count that means a width.
    pub fn truncate(&self, text: &str, px: f32, max_width: i32) -> String {
        if max_width <= 0 {
            return String::new();
        }
        if self.measure(text, px) <= max_width {
            return text.to_string();
        }
        let ellipsis = '…';
        let ellipsis_width = self.font.metrics(ellipsis, px).advance_width;
        // Not even the ellipsis fits; showing it alone says nothing that
        // showing nothing does not.
        if ellipsis_width.ceil() as i32 > max_width {
            return String::new();
        }

        let mut kept = String::new();
        let mut width = 0.0f32;
        for character in text.chars() {
            let advance = self.font.metrics(character, px).advance_width;
            if (width + advance + ellipsis_width).ceil() as i32 > max_width {
                break;
            }
            width += advance;
            kept.push(character);
        }
        kept.push(ellipsis);
        kept
    }

    /// Rasterise a string to a premultiplied RGBA image.
    ///
    /// `None` for anything with no pixels — an empty string, or one that is
    /// all spaces — so callers can skip the upload rather than handle a
    /// zero-sized texture.
    pub fn render(&mut self, text: &str, px: f32, colour: [f32; 4]) -> Option<&Image> {
        let key = (text.to_string(), px.to_bits());
        if !self.cache.contains_key(&key) {
            let image = self.rasterise(text, px, colour);
            self.cache.insert(key.clone(), image);
        }
        self.cache.get(&key)?.as_ref()
    }

    fn rasterise(&self, text: &str, px: f32, colour: [f32; 4]) -> Option<Image> {
        let width = self.measure(text, px);
        let height = self.line_height(px);
        if width <= 0 || height <= 0 {
            return None;
        }

        let ascent = self
            .font
            .horizontal_line_metrics(px)
            .map(|m| m.ascent)
            .unwrap_or(px);
        let mut data = vec![0u8; (width * height * 4) as usize];
        let mut pen = 0.0f32;

        for character in text.chars() {
            let (metrics, coverage) = self.font.rasterize(character, px);

            for gy in 0..metrics.height {
                for gx in 0..metrics.width {
                    let alpha = coverage[gy * metrics.width + gx];
                    if alpha == 0 {
                        continue;
                    }
                    let x = pen.round() as i32 + metrics.xmin + gx as i32;
                    // `ymin` is the offset of the bitmap's *bottom* edge from
                    // the baseline, so the top is the baseline minus the
                    // height above it. Getting this backwards flips every
                    // glyph about its own baseline, which reads as a broken
                    // font rather than a sign error.
                    let y = (ascent.round() as i32) - metrics.ymin - metrics.height as i32
                        + gy as i32;
                    if x < 0 || y < 0 || x >= width || y >= height {
                        continue;
                    }

                    let a = colour[3] * (alpha as f32 / 255.0);
                    let i = ((y * width + x) * 4) as usize;
                    // Premultiplied, matching every other texture cusk
                    // uploads. Straight alpha here would halo the text.
                    data[i] = (colour[0] * a * 255.0) as u8;
                    data[i + 1] = (colour[1] * a * 255.0) as u8;
                    data[i + 2] = (colour[2] * a * 255.0) as u8;
                    data[i + 3] = (a * 255.0) as u8;
                }
            }
            pen += metrics.advance_width;
        }

        if data.chunks(4).all(|p| p[3] == 0) {
            return None;
        }
        Some(Image::new(width as u32, height as u32, data))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn face() -> Option<Face> {
        find_font("").and_then(|path| Face::load(&path))
    }

    /// Every test below needs a real font. On a machine without one they skip
    /// rather than fail — a missing system font is not a defect in this code,
    /// and a red suite would train someone to ignore it.
    macro_rules! face_or_skip {
        () => {
            match face() {
                Some(face) => face,
                None => {
                    eprintln!("no system font; skipping");
                    return;
                }
            }
        };
    }

    #[test]
    fn a_font_is_found_on_this_system() {
        assert!(find_font("").is_some(), "no candidate font exists");
    }

    /// A configured font that does not exist must not be silently replaced —
    /// the user would be looking at the wrong typeface with nothing said.
    #[test]
    fn a_configured_font_that_is_missing_is_not_replaced() {
        assert_eq!(find_font("/nonexistent/font.ttf"), None);
    }

    #[test]
    fn an_empty_setting_falls_back_to_the_system() {
        assert!(find_font("   ").is_some());
    }

    #[test]
    fn wider_strings_measure_wider() {
        let face = face_or_skip!();
        let short = face.measure("i", 16.0);
        let long = face.measure("iiiiiiiiii", 16.0);
        assert!(long > short);
        assert_eq!(face.measure("", 16.0), 0);
    }

    /// A space has no ink but must still take room, or every title closes up.
    #[test]
    fn spaces_take_width() {
        let face = face_or_skip!();
        assert!(face.measure("a a", 16.0) > face.measure("aa", 16.0));
    }

    #[test]
    fn a_bigger_size_measures_wider_and_taller() {
        let face = face_or_skip!();
        assert!(face.measure("Hello", 32.0) > face.measure("Hello", 16.0));
        assert!(face.line_height(32.0) > face.line_height(16.0));
    }

    #[test]
    fn text_that_fits_is_not_truncated() {
        let face = face_or_skip!();
        let text = "Terminal";
        let width = face.measure(text, 14.0);
        assert_eq!(face.truncate(text, 14.0, width + 10), text);
    }

    /// The result must actually fit, or truncation has done nothing except
    /// change the string.
    #[test]
    fn truncated_text_fits_the_budget() {
        let face = face_or_skip!();
        let text = "A window title that is far too long for the space available";
        for budget in [40, 80, 160, 300] {
            let cut = face.truncate(text, 14.0, budget);
            assert!(
                face.measure(&cut, 14.0) <= budget,
                "{budget}px budget produced {:?} at {}px",
                cut,
                face.measure(&cut, 14.0)
            );
            assert!(cut.chars().count() < text.chars().count());
        }
    }

    #[test]
    fn a_budget_too_small_for_anything_yields_nothing() {
        let face = face_or_skip!();
        assert_eq!(face.truncate("something", 14.0, 0), "");
        assert_eq!(face.truncate("something", 14.0, -5), "");
    }

    #[test]
    fn rendering_produces_an_image_of_the_measured_size() {
        let mut face = face_or_skip!();
        let image = face.render("Hello", 16.0, [1.0, 1.0, 1.0, 1.0]).cloned();
        let image = image.expect("visible text should rasterise");
        assert_eq!(image.width as i32, face.measure("Hello", 16.0));
        assert_eq!(image.height as i32, face.line_height(16.0));
        assert_eq!(image.data.len(), (image.width * image.height * 4) as usize);
    }

    /// Ink somewhere, and not everywhere: all-transparent means nothing was
    /// drawn, all-opaque means the glyph bitmaps were pasted as blocks.
    #[test]
    fn rendered_text_has_ink_but_is_not_a_block() {
        let mut face = face_or_skip!();
        let image = face.render("Hello", 20.0, [1.0, 1.0, 1.0, 1.0]).cloned().unwrap();
        let inked = image.data.chunks(4).filter(|p| p[3] > 0).count();
        let total = (image.width * image.height) as usize;
        assert!(inked > 0, "nothing drawn");
        assert!(inked < total, "everything drawn");
    }

    /// Straight alpha would halo the text against the panel.
    #[test]
    fn rendered_text_is_premultiplied() {
        let mut face = face_or_skip!();
        let image = face.render("Wg", 18.0, [1.0, 1.0, 1.0, 1.0]).cloned().unwrap();
        for p in image.data.chunks(4) {
            assert!(p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3], "{p:?}");
        }
    }

    /// Glyphs must sit on a shared baseline. If `ymin` is applied with the
    /// wrong sign each glyph flips about its own baseline, and a descender is
    /// the clearest way to see it: "g" must reach below where "o" stops.
    #[test]
    fn descenders_sit_below_the_baseline_of_round_letters() {
        let mut face = face_or_skip!();
        let lowest = |face: &mut Face, text: &str| -> u32 {
            let image = face.render(text, 40.0, [1.0, 1.0, 1.0, 1.0]).cloned().unwrap();
            let mut lowest = 0;
            for y in 0..image.height {
                for x in 0..image.width {
                    if image.pixel(x, y)[3] > 0 {
                        lowest = lowest.max(y);
                    }
                }
            }
            lowest
        };
        assert!(
            lowest(&mut face, "g") > lowest(&mut face, "o"),
            "a descender must reach below a round letter"
        );
    }

    #[test]
    fn nothing_visible_renders_to_nothing() {
        let mut face = face_or_skip!();
        assert!(face.render("", 16.0, [1.0; 4]).is_none());
        assert!(face.render("   ", 16.0, [1.0; 4]).is_none());
    }

    /// The cache must return the same image, not a differently-sized one.
    #[test]
    fn the_cache_returns_a_matching_image() {
        let mut face = face_or_skip!();
        let first = face.render("cached", 16.0, [1.0; 4]).cloned().unwrap();
        let second = face.render("cached", 16.0, [1.0; 4]).cloned().unwrap();
        assert_eq!(first, second);
    }
}
