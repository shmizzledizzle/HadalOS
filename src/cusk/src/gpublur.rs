//! Blurring what is actually behind a window.
//!
//! Milestone 7 blurs the *wallpaper* and draws it behind each window. That is
//! most of the effect and costs nothing per frame, but it is a lie whenever two
//! windows overlap: the top one shows blurred wallpaper where it should show
//! the window underneath.
//!
//! This blurs the composited scene instead, on the GPU, every frame.
//!
//! # Built up in order, not blurred all at once
//!
//! The obvious implementation — composite everything, blur it, draw windows
//! over the result — is wrong in a way that looks like a feedback loop: a
//! window's own pixels are in the blur behind it.
//!
//! So the scene is assembled back to front into an offscreen texture, and each
//! window blurs the texture *as it stands before that window is drawn*. What
//! ends up behind a window is exactly what is behind it.
//!
//! # Why this is safe API and not raw GL
//!
//! `GlesFrame::with_context` hands out the raw context, and smithay is explicit
//! that any state changed there must be restored or the renderer misbehaves in
//! ways that surface far from the cause. `Offscreen::create_buffer` and
//! `Bind::bind` do the same job through the API the renderer maintains, so
//! there is no state to restore and no unsafe block to get wrong.
//!
//! # Cost
//!
//! One downsample plus `passes` blur steps at half resolution, per blurring
//! window, per frame. That is why `appearance.window-blur` exists and why it
//! defaults off: it is real work, and the wallpaper blur it replaces is free.

use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{
    GlesRenderer, GlesTexProgram, GlesTexture, Uniform, UniformName, UniformType,
};
use smithay::backend::renderer::{Bind, Frame, Offscreen, Renderer};
use smithay::utils::{Buffer, Physical, Point, Rectangle, Size, Transform};

/// A five-tap Kawase step.
///
/// Cheaper than a Gaussian of the same visual width, and separable-free: the
/// four diagonal taps plus the centre, with the offset growing each pass, is
/// what spreads the blur without a wide kernel. Repeated, it converges on
/// something indistinguishable from a Gaussian.
const BLUR_SHADER: &str = r#"#version 100

//_DEFINES_

precision mediump float;

uniform sampler2D tex;
uniform float alpha;
varying vec2 v_coords;

uniform vec2 offset;

void main() {
    vec4 sum = texture2D(tex, v_coords) * 4.0;
    sum += texture2D(tex, v_coords + vec2(-offset.x, -offset.y));
    sum += texture2D(tex, v_coords + vec2( offset.x, -offset.y));
    sum += texture2D(tex, v_coords + vec2(-offset.x,  offset.y));
    sum += texture2D(tex, v_coords + vec2( offset.x,  offset.y));
    gl_FragColor = (sum / 8.0) * alpha;
}
"#;

pub struct GpuBlur {
    program: Option<GlesTexProgram>,
    /// The scene as it is being assembled, at full size.
    scene: Option<GlesTexture>,
    /// Half-size ping-pong pair.
    ping: Option<GlesTexture>,
    pong: Option<GlesTexture>,
    size: (i32, i32),
}

impl GpuBlur {
    pub fn new(renderer: &mut GlesRenderer) -> Self {
        let program = match renderer
            .compile_custom_texture_shader(BLUR_SHADER, &[UniformName::new("offset", UniformType::_2f)])
        {
            Ok(program) => Some(program),
            Err(e) => {
                // Not load-bearing: without it, window blur simply never
                // engages and the wallpaper blur carries on.
                tracing::warn!("blur shader did not compile ({e}); window blur disabled");
                None
            }
        };
        GpuBlur { program, scene: None, ping: None, pong: None, size: (0, 0) }
    }

    /// Allocate, or reallocate on a size change.
    fn ensure(&mut self, renderer: &mut GlesRenderer, size: (i32, i32)) -> bool {
        if self.size == size && self.scene.is_some() {
            return true;
        }
        let (w, h) = (size.0.max(1), size.1.max(1));
        let (hw, hh) = ((w / 2).max(1), (h / 2).max(1));

        let make = |renderer: &mut GlesRenderer, w: i32, h: i32| {
            Offscreen::<GlesTexture>::create_buffer(renderer, Fourcc::Abgr8888, Size::from((w, h))).ok()
        };
        self.scene = make(renderer, w, h);
        self.ping = make(renderer, hw, hh);
        self.pong = make(renderer, hw, hh);
        self.size = size;
        self.scene.is_some() && self.ping.is_some() && self.pong.is_some()
    }

    /// Start a frame: allocate if needed and hand back the scene target.
    pub fn begin(&mut self, renderer: &mut GlesRenderer, size: (i32, i32)) -> bool {
        self.program.is_some() && self.ensure(renderer, size)
    }

    /// Hand the scene texture out so the caller can render into it.
    ///
    /// Taken rather than borrowed because the caller needs `&mut` on the
    /// texture while this struct still owns the ping-pong pair — two mutable
    /// borrows of one struct otherwise. Put it back before blurring.
    pub fn take_scene(&mut self) -> Option<GlesTexture> {
        self.scene.take()
    }

    pub fn put_scene(&mut self, scene: GlesTexture) {
        self.scene = Some(scene);
    }

    pub fn scene_ref(&self) -> Option<&GlesTexture> {
        self.scene.as_ref()
    }

    /// The most recent blur result, at half size.
    pub fn blurred(&self) -> Option<&GlesTexture> {
        self.ping.as_ref()
    }

    /// Blur the scene as it currently stands, returning the half-size result.
    ///
    /// `radius` is in full-resolution pixels; the offset is halved with the
    /// resolution so the number means the same thing as the wallpaper blur's.
    pub fn blur_scene(
        &mut self,
        renderer: &mut GlesRenderer,
        radius: i32,
        passes: u32,
    ) -> Option<&GlesTexture> {
        let program = self.program.clone()?;
        let (w, h) = self.size;
        let (hw, hh) = ((w / 2).max(1), (h / 2).max(1));
        let half = Size::<i32, Physical>::from((hw, hh));

        // Downsample the scene into `ping`. Halving first is what makes the
        // rest cheap, and costs nothing visually because everything after it
        // is a low-pass filter.
        {
            let scene = self.scene.as_ref()?;
            let mut ping = self.ping.take()?;
            {
                let mut target = renderer.bind(&mut ping).ok()?;
                let mut frame = renderer.render(&mut target, half, Transform::Normal).ok()?;
                let src = Rectangle::<f64, Buffer>::from_size(Size::from((w as f64, h as f64)));
                let _ = Frame::render_texture_from_to(
                    &mut frame,
                    scene,
                    src,
                    Rectangle::from_size(half),
                    &[Rectangle::from_size(half)],
                    &[],
                    Transform::Normal,
                    1.0,
                );
                let _ = frame.finish();
            }
            self.ping = Some(ping);
        }

        // Ping-pong, widening the offset each pass. A constant offset just
        // blurs the same distance repeatedly and stops spreading.
        let steps = passes.clamp(1, 6);
        for step in 0..steps {
            let spread = (radius.max(1) as f32 / 2.0) * (step as f32 + 1.0) / steps as f32;
            let offset = (spread / hw as f32, spread / hh as f32);

            let source = self.ping.take()?;
            let mut destination = self.pong.take()?;
            {
                let mut target = renderer.bind(&mut destination).ok()?;
                let mut frame = renderer.render(&mut target, half, Transform::Normal).ok()?;
                let src = Rectangle::<f64, Buffer>::from_size(Size::from((hw as f64, hh as f64)));
                let _ = frame.render_texture_from_to(
                    &source,
                    src,
                    Rectangle::from_size(half),
                    &[Rectangle::from_size(half)],
                    &[],
                    Transform::Normal,
                    1.0,
                    Some(&program),
                    &[Uniform::new("offset", offset)],
                );
                let _ = frame.finish();
            }
            // Swapped rather than copied: the destination becomes the source of
            // the next pass, which is the whole point of a ping-pong.
            self.ping = Some(destination);
            self.pong = Some(source);
        }

        self.ping.as_ref()
    }

    /// The source rectangle in the half-size blurred texture for a screen rect.
    pub fn half_src(rect: Rectangle<i32, Physical>) -> Rectangle<f64, Buffer> {
        Rectangle::new(
            Point::from((rect.loc.x as f64 / 2.0, rect.loc.y as f64 / 2.0)),
            Size::from((rect.size.w as f64 / 2.0, rect.size.h as f64 / 2.0)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rect(x: i32, y: i32, w: i32, h: i32) -> Rectangle<i32, Physical> {
        Rectangle::new(Point::from((x, y)), Size::from((w, h)))
    }

    /// The blurred texture is half size, so a screen rectangle has to be
    /// halved to index it. Getting this wrong samples the wrong part of the
    /// screen, which looks like the blur lagging behind the window.
    #[test]
    fn a_screen_rect_maps_to_half_coordinates() {
        let src = GpuBlur::half_src(rect(200, 100, 400, 300));
        assert_eq!(src.loc.x, 100.0);
        assert_eq!(src.loc.y, 50.0);
        assert_eq!(src.size.w, 200.0);
        assert_eq!(src.size.h, 150.0);
    }

    /// Odd coordinates must not be rounded to integers here — the source rect
    /// is in floats precisely so a window at an odd position still samples the
    /// right place rather than drifting half a pixel per frame.
    #[test]
    fn odd_coordinates_keep_their_half_pixel() {
        let src = GpuBlur::half_src(rect(41, 0, 101, 0));
        assert_eq!(src.loc.x, 20.5);
        assert_eq!(src.size.w, 50.5);
    }

    #[test]
    fn the_origin_maps_to_the_origin() {
        let src = GpuBlur::half_src(rect(0, 0, 1920, 1080));
        assert_eq!((src.loc.x, src.loc.y), (0.0, 0.0));
        assert_eq!((src.size.w, src.size.h), (960.0, 540.0));
    }
}
