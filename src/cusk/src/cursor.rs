//! The pointer.
//!
//! Carried unfinished since milestone 1, where `cursor_image` was a no-op and
//! the note said cursor shape "says nothing about whether pointer routing
//! works". That was true and it was also the wrong thing to leave: cusk drew no
//! pointer at all, so every gesture it has — click to focus, drag to move,
//! drag the divider, drop a tile onto another — was aimed blind. Nested inside
//! another session the host's cursor covered for it. On a tty there would be
//! nothing on screen at all.
//!
//! # Drawn, not loaded
//!
//! The usual source is an XCursor theme from the filesystem. That means a theme
//! search path, a name lookup, a fallback chain, and a set of failure modes
//! that all end in "no pointer" — the exact outcome being fixed. A cursor drawn
//! in code is always available, has no configuration to get wrong, and is a
//! pure function from a size to a bitmap, so it can be tested without a GPU or
//! a session.
//!
//! Client-provided cursors are still honoured: a terminal that asks for an
//! I-beam gets its own surface rendered. This is only the fallback, which is
//! what a compositor owes when no client has an opinion.

use crate::wallpaper::Image;

/// A cursor bitmap and the point within it that is "the pointer".
#[derive(Debug, Clone)]
pub struct Cursor {
    pub image: Image,
    /// Offset from the image's top-left to the pixel the pointer actually
    /// points at. Subtracted from the pointer position when drawing; get it
    /// wrong and everything is clicked from the wrong place, which reads as
    /// broken hit-testing rather than a misplaced drawing.
    pub hotspot: (i32, i32),
}

/// The outline of a classic arrow, in units of a 24-unit grid.
///
/// Kept as data rather than drawing commands so the shape can be scaled to any
/// size by one multiplication, and so the tip is provably at the origin — the
/// hotspot depends on it.
const ARROW: &[(f32, f32)] = &[
    (0.0, 0.0),
    (0.0, 16.8),
    (4.2, 12.9),
    (7.0, 19.9),
    (10.1, 18.6),
    (7.4, 11.9),
    (13.0, 11.6),
];

/// Draw the fallback arrow at the given height in pixels.
pub fn arrow(size: u32) -> Cursor {
    let size = size.clamp(8, 128);
    let scale = size as f32 / 24.0;
    let points: Vec<(f32, f32)> = ARROW.iter().map(|(x, y)| (x * scale, y * scale)).collect();

    let width = size;
    let height = size;
    let mut data = vec![0u8; (width * height * 4) as usize];

    for y in 0..height {
        for x in 0..width {
            // Sampled at the pixel centre. Sampling at the corner puts the
            // whole shape half a pixel up and left, which at 24px is visible.
            let p = (x as f32 + 0.5, y as f32 + 0.5);
            let inside = contains(&points, p);
            // The border is what makes the arrow readable over both a dark
            // wallpaper and a white window. A single-colour cursor disappears
            // against something, always.
            let edge = !inside && near_edge(&points, p, 1.2 * scale.max(1.0));

            let i = ((y * width + x) * 4) as usize;
            let (r, g, b, a) = if inside {
                (255, 255, 255, 255)
            } else if edge {
                (0, 0, 0, 220)
            } else {
                (0, 0, 0, 0)
            };
            // Premultiplied, which is what the renderer blends. Storing
            // straight alpha here makes the outline haloed rather than crisp.
            data[i] = (r as u32 * a as u32 / 255) as u8;
            data[i + 1] = (g as u32 * a as u32 / 255) as u8;
            data[i + 2] = (b as u32 * a as u32 / 255) as u8;
            data[i + 3] = a;
        }
    }

    Cursor {
        image: Image::new(width, height, data),
        // The first point of the outline is the tip, and it is at the origin.
        hotspot: (0, 0),
    }
}

/// Even-odd point-in-polygon.
fn contains(points: &[(f32, f32)], p: (f32, f32)) -> bool {
    let mut inside = false;
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        let (xi, yi) = points[i];
        let (xj, yj) = points[j];
        if (yi > p.1) != (yj > p.1) && p.0 < (xj - xi) * (p.1 - yi) / (yj - yi) + xi {
            inside = !inside;
        }
        j = i;
    }
    inside
}

/// Whether a point lies within `width` of the outline.
fn near_edge(points: &[(f32, f32)], p: (f32, f32), width: f32) -> bool {
    let mut j = points.len() - 1;
    for i in 0..points.len() {
        if distance_to_segment(p, points[j], points[i]) <= width {
            return true;
        }
        j = i;
    }
    false
}

fn distance_to_segment(p: (f32, f32), a: (f32, f32), b: (f32, f32)) -> f32 {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len_sq = dx * dx + dy * dy;
    if len_sq <= f32::EPSILON {
        return ((p.0 - a.0).powi(2) + (p.1 - a.1).powi(2)).sqrt();
    }
    let t = (((p.0 - a.0) * dx + (p.1 - a.1) * dy) / len_sq).clamp(0.0, 1.0);
    let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
    ((p.0 - cx).powi(2) + (p.1 - cy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn alpha_at(cursor: &Cursor, x: u32, y: u32) -> u8 {
        cursor.image.pixel(x, y)[3]
    }

    #[test]
    fn the_arrow_is_the_size_asked_for() {
        let c = arrow(24);
        assert_eq!((c.image.width, c.image.height), (24, 24));
        assert_eq!(c.image.data.len(), 24 * 24 * 4);
    }

    /// The hotspot must be the tip. Anywhere else and every click lands at an
    /// offset from where the user aimed — which looks like broken hit-testing
    /// rather than a misplaced drawing.
    #[test]
    fn the_hotspot_is_the_tip_and_the_tip_is_drawn() {
        let c = arrow(24);
        assert_eq!(c.hotspot, (0, 0));
        assert!(alpha_at(&c, 0, 0) > 0, "nothing drawn at the hotspot");
        assert!(alpha_at(&c, 1, 1) > 0, "the tip should be solid");
    }

    /// A cursor that is opaque everywhere is a square, and a cursor that is
    /// transparent everywhere is nothing. Both are easy to produce by getting
    /// the polygon test backwards.
    #[test]
    fn the_arrow_is_neither_empty_nor_a_block() {
        let c = arrow(24);
        let opaque = c.image.data.chunks(4).filter(|p| p[3] > 0).count();
        let total = (c.image.width * c.image.height) as usize;
        assert!(opaque > total / 12, "too little drawn: {opaque}/{total}");
        assert!(opaque < total * 3 / 4, "too much drawn: {opaque}/{total}");
    }

    #[test]
    fn the_far_corner_is_transparent() {
        let c = arrow(24);
        assert_eq!(alpha_at(&c, 23, 0), 0, "top right must be empty");
        assert_eq!(alpha_at(&c, 23, 23), 0, "bottom right must be empty");
    }

    /// White fill over a black outline is what keeps the pointer visible on
    /// both a dark wallpaper and a white window.
    #[test]
    fn the_arrow_has_a_white_body_and_a_dark_outline() {
        let c = arrow(32);
        let mut white = 0;
        let mut dark = 0;
        for p in c.image.data.chunks(4) {
            if p[3] == 0 {
                continue;
            }
            if p[0] > 200 {
                white += 1;
            } else if p[0] < 60 {
                dark += 1;
            }
        }
        assert!(white > 0, "no body");
        assert!(dark > 0, "no outline");
    }

    /// Premultiplied, or the outline blends as a halo instead of a line.
    #[test]
    fn colours_are_premultiplied() {
        let c = arrow(24);
        for p in c.image.data.chunks(4) {
            assert!(
                p[0] <= p[3] && p[1] <= p[3] && p[2] <= p[3],
                "channel exceeds alpha: {p:?}"
            );
        }
    }

    /// Scaling must not panic or produce a degenerate bitmap at any size a
    /// HiDPI output might ask for.
    #[test]
    fn every_reasonable_size_produces_a_usable_arrow() {
        for size in [8, 16, 24, 32, 48, 64, 96, 128] {
            let c = arrow(size);
            assert_eq!(c.image.width, size);
            assert!(
                c.image.data.chunks(4).any(|p| p[3] > 0),
                "size {size} drew nothing"
            );
            assert!(alpha_at(&c, 0, 0) > 0, "size {size} has no tip");
        }
    }

    #[test]
    fn absurd_sizes_are_clamped_rather_than_trusted() {
        assert_eq!(arrow(0).image.width, 8);
        assert_eq!(arrow(100_000).image.width, 128);
    }
}
