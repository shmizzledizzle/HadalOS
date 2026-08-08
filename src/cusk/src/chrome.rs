//! Window chrome: rounded corners and focus rings.
//!
//! The two visual moves the reference shells are built on, and they are drawn
//! by different mechanisms because they are different problems.
//!
//! # Rounded corners are subtractive
//!
//! A client draws a rectangle. The compositor cannot ask it not to, and
//! clipping the window's own texture would mean routing every surface through
//! a custom shader — which means rebuilding what
//! `render_elements_from_surface_tree` does for subsurfaces and popups.
//!
//! So the corners are *painted back over* instead: after a window is drawn,
//! four small quads of the sharp wallpaper go on top of its square corners,
//! through a shader that keeps only the sliver lying outside the corner arc.
//! The window appears rounded, and what shows through is the desktop behind it,
//! which is what a rounded corner is.
//!
//! This works only because `wallpaper::load_scaled` produces a texture at
//! exactly the output size: a screen rectangle is its own source crop, so the
//! shader can recover a pixel's screen position from its texture coordinate
//! and there is no second coordinate space to get wrong.
//!
//! # Focus rings are additive
//!
//! Nothing needs to be removed, so the ring is one quad in the band around the
//! window, with a signed-distance field picking out the area between two
//! rounded rectangles. It is transparent everywhere else, so it never covers
//! window content — a focus ring that dims the edge of the thing it is
//! highlighting is worse than none.
//!
//! # Both degrade rather than fail
//!
//! Shader compilation can fail — an old driver, a strict ES parser, a
//! software rasteriser with its own opinions. Neither is load-bearing: if
//! either program fails to compile it is reported once, left as `None`, and
//! cusk runs with square corners and no ring. A compositor that refuses to
//! start because it could not round a corner is a worse outcome than one that
//! looks plainer than intended.

use smithay::backend::renderer::gles::{
    GlesFrame, GlesPixelProgram, GlesRenderer, GlesTexProgram, Uniform, UniformName, UniformType,
};
use smithay::utils::{Buffer, Logical, Physical, Point, Rectangle, Size, Transform};

use cusk::theme;

/// Keeps only the part of the texture lying *outside* a corner arc.
///
/// `//_DEFINES_` is required: smithay substitutes `#define` directives there
/// and compiles three variants. This shader ignores all of them — it is only
/// ever used on an ordinary internal texture — but the marker has to be
/// present or the substitution silently does nothing and the variants are
/// identical.
const CORNER_SHADER: &str = r#"#version 100

//_DEFINES_

precision mediump float;

uniform sampler2D tex;
uniform float alpha;
varying vec2 v_coords;

uniform vec2 tex_size;
uniform vec2 arc;
uniform float radius;

void main() {
    // Texture coordinates are screen coordinates here, because the backdrop is
    // built at exactly the output size.
    vec2 p = v_coords * tex_size;
    float d = distance(p, arc);

    // One pixel of feathering either side of the arc. Without it the corner is
    // a staircase, which at a 12px radius is the most visible artefact on
    // screen.
    float outside = smoothstep(radius - 1.0, radius + 1.0, d);

    gl_FragColor = texture2D(tex, v_coords) * alpha * outside;
}
"#;

/// The band between two rounded rectangles.
///
/// `compile_custom_pixel_shader` prepends `#version 100`, so this must not.
const RING_SHADER: &str = r#"
precision mediump float;

uniform vec2 size;
uniform float alpha;
varying vec2 v_coords;

uniform vec4 ring_color;
uniform float radius;
uniform float width;

// Signed distance to a rounded rectangle centred on the origin: negative
// inside, positive outside, and in pixels either way, which is what makes the
// one-pixel feathering below correct at any size.
float rounded_box(vec2 p, vec2 half_size, float r) {
    vec2 q = abs(p) - half_size + r;
    return length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - r;
}

void main() {
    vec2 half_size = size * 0.5;
    vec2 p = v_coords * size - half_size;

    float outer = rounded_box(p, half_size, radius);
    float inner = rounded_box(p, half_size - vec2(width), max(radius - width, 0.0));

    // Inside the outer edge and outside the inner one. Feathered on both, so
    // the ring is smooth against the wallpaper and against the window.
    float coverage = (1.0 - smoothstep(-1.0, 1.0, outer)) * smoothstep(-1.0, 1.0, inner);

    // Premultiplied, which is what the renderer blends.
    float a = ring_color.a * coverage * alpha;
    gl_FragColor = vec4(ring_color.rgb * a, a);
}
"#;

pub struct Chrome {
    corner: Option<GlesTexProgram>,
    ring: Option<GlesPixelProgram>,
}

impl Chrome {
    pub fn new(renderer: &mut GlesRenderer) -> Self {
        let corner = match renderer.compile_custom_texture_shader(
            CORNER_SHADER,
            &[
                UniformName::new("tex_size", UniformType::_2f),
                UniformName::new("arc", UniformType::_2f),
                UniformName::new("radius", UniformType::_1f),
            ],
        ) {
            Ok(program) => Some(program),
            Err(e) => {
                tracing::warn!("corner shader did not compile ({e}); corners stay square");
                None
            }
        };

        let ring = match renderer.compile_custom_pixel_shader(
            RING_SHADER,
            &[
                UniformName::new("ring_color", UniformType::_4f),
                UniformName::new("radius", UniformType::_1f),
                UniformName::new("width", UniformType::_1f),
            ],
        ) {
            Ok(program) => Some(program),
            Err(e) => {
                tracing::warn!("ring shader did not compile ({e}); no focus ring");
                None
            }
        };

        Self { corner, ring }
    }

    /// Paint the backdrop back over a window's four square corners.
    ///
    /// Four small quads rather than one window-sized one. The shader is cheap
    /// but this runs on a software rasteriser, and a full-window fragment pass
    /// per window per frame is a real cost where four 12x12 patches is not.
    #[allow(clippy::too_many_arguments)]
    pub fn round_corners(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        backdrop: &smithay::backend::renderer::gles::GlesTexture,
        window: Rectangle<i32, Logical>,
        radius: i32,
        output: Size<i32, Logical>,
    ) {
        let (Some(program), true) = (&self.corner, radius > 0) else { return };

        let tex_size = (output.w as f32, output.h as f32);
        let r = radius as f32;

        for (corner, arc) in corner_patches(window, radius) {
            let Some(patch) = corner.intersection(Rectangle::from_size(output)) else {
                continue;
            };
            let src = Rectangle::<f64, Buffer>::new(
                Point::from((patch.loc.x as f64, patch.loc.y as f64)),
                Size::from((patch.size.w as f64, patch.size.h as f64)),
            );
            let dst = Rectangle::<i32, Physical>::new(
                Point::from((patch.loc.x, patch.loc.y)),
                Size::from((patch.size.w, patch.size.h)),
            );
            let _ = frame.render_texture_from_to(
                backdrop,
                src,
                dst,
                &[dst],
                &[],
                Transform::Normal,
                1.0,
                Some(program),
                &[
                    Uniform::new("tex_size", tex_size),
                    Uniform::new("arc", (arc.0 as f32, arc.1 as f32)),
                    Uniform::new("radius", r),
                ],
            );
        }
    }

    /// Draw the ring in the band just outside a window.
    pub fn focus_ring(
        &self,
        frame: &mut GlesFrame<'_, '_>,
        window: Rectangle<i32, Logical>,
        radius: i32,
        width: i32,
        colour: theme::Rgba,
    ) {
        let (Some(program), true) = (&self.ring, width > 0) else { return };

        // The band lies entirely outside the window, so the ring never covers
        // content — highlighting a window by dimming its edge is worse than
        // not highlighting it.
        let band = Rectangle::<i32, Logical>::new(
            Point::from((window.loc.x - width, window.loc.y - width)),
            Size::from((window.size.w + width * 2, window.size.h + width * 2)),
        );
        let dst = Rectangle::<i32, Physical>::new(
            Point::from((band.loc.x, band.loc.y)),
            Size::from((band.size.w, band.size.h)),
        );
        let size = Size::<i32, Buffer>::from((band.size.w, band.size.h));

        let _ = frame.render_pixel_shader_to(
            program,
            Rectangle::from_size(Size::from((band.size.w as f64, band.size.h as f64))),
            dst,
            size,
            None,
            1.0,
            &[
                Uniform::new("ring_color", colour),
                // The ring's own corner radius is the window's plus the width
                // it sits outside of, or the ring and the corner drift apart.
                Uniform::new("radius", (radius + width) as f32),
                Uniform::new("width", width as f32),
            ],
        );
    }
}

/// The four corner patches of a rectangle, each with the centre of its arc.
///
/// Separated out because it is the part with the arithmetic in it, and the
/// only part that can be checked without a GL context.
pub fn corner_patches(
    rect: Rectangle<i32, Logical>,
    radius: i32,
) -> [(Rectangle<i32, Logical>, (i32, i32)); 4] {
    let r = radius;
    let (x, y, w, h) = (rect.loc.x, rect.loc.y, rect.size.w, rect.size.h);
    let patch = |px: i32, py: i32| {
        Rectangle::<i32, Logical>::new(Point::from((px, py)), Size::from((r, r)))
    };
    [
        (patch(x, y), (x + r, y + r)),
        (patch(x + w - r, y), (x + w - r, y + r)),
        (patch(x, y + h - r), (x + r, y + h - r)),
        (patch(x + w - r, y + h - r), (x + w - r, y + h - r)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Logical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    #[test]
    fn the_patches_sit_in_the_four_corners() {
        let patches = corner_patches(rect(100, 200, 400, 300), 12);
        let corners: Vec<(i32, i32)> = patches.iter().map(|(p, _)| (p.loc.x, p.loc.y)).collect();
        assert_eq!(corners, vec![(100, 200), (488, 200), (100, 488), (488, 488)]);
        for (patch, _) in patches {
            assert_eq!((patch.size.w, patch.size.h), (12, 12));
        }
    }

    /// Each arc centre must be exactly one radius inside its own corner, or the
    /// rounding is lopsided in a way that is obvious on screen and invisible
    /// in the code.
    #[test]
    fn each_arc_is_one_radius_inside_its_corner() {
        let r = 16;
        let window = rect(0, 0, 200, 100);
        for (patch, arc) in corner_patches(window, r) {
            let nearest_x = if patch.loc.x == window.loc.x { window.loc.x } else { window.loc.x + window.size.w };
            let nearest_y = if patch.loc.y == window.loc.y { window.loc.y } else { window.loc.y + window.size.h };
            assert_eq!((arc.0 - nearest_x).abs(), r, "arc x for patch at {:?}", patch.loc);
            assert_eq!((arc.1 - nearest_y).abs(), r, "arc y for patch at {:?}", patch.loc);
        }
    }

    /// Every patch must lie inside the window. One that overhangs would paint
    /// the wallpaper over whatever is beside the window, not over the window.
    #[test]
    fn patches_stay_within_the_window() {
        let window = rect(30, 40, 300, 200);
        for (patch, _) in corner_patches(window, 20) {
            assert!(window.contains_rect(patch), "{patch:?} escapes {window:?}");
        }
    }

    /// A radius larger than the window would make opposite patches overlap and
    /// erase the middle of the window. The renderer clamps before calling, and
    /// this pins what it must clamp to.
    #[test]
    fn a_radius_of_half_the_window_still_tiles_the_corners() {
        let window = rect(0, 0, 40, 40);
        let patches = corner_patches(window, 20);
        for (patch, _) in patches {
            assert!(window.contains_rect(patch));
        }
    }
}
