//! Thumbnails of minimised windows.
//!
//! A minimised window is not rendered into the session's frame, so there is
//! nothing on screen to sample. What makes a thumbnail possible anyway is that
//! the client's last committed buffer stays valid until it attaches another —
//! and a minimised client is not drawing — so the compositor can render that
//! window again, on its own, whenever it likes.
//!
//! That is what `capture_window` in the compositor does, into an offscreen
//! texture it owns. The first version instead copied the window's rectangle out
//! of the frame that had just been presented; it produced a correct thumbnail
//! and then killed the compositor, because reading the backend's own
//! framebuffer invalidates the bind it was about to present with.
//!
//! The consequences for this module:
//!
//! - Capture happens in the render loop, because that is where the renderer is
//!   — not because the window has to still be visible. It does not.
//! - Snapshots are downscaled *before* being stored. A 1920×1080 window is 8 MB
//!   of RGBA; twenty minimised windows would be 160 MB held to draw twenty
//!   thumbnails a couple of hundred pixels wide. Downscaling at capture makes
//!   the cost proportional to what is displayed rather than to what was hidden.
//! - Nothing here touches Wayland or the renderer. The scaling is arithmetic
//!   over a byte slice, which is the part with the bugs in it, and it is
//!   testable without a compositor.

use std::collections::HashMap;

use crate::foreign_toplevel::ToplevelId;

/// The longest edge a stored thumbnail may have, in pixels.
///
/// Sized for a dock tile on a HiDPI screen with room to spare, not for the
/// window it came from. The point of the cap is that memory tracks the number
/// of thumbnails rather than the resolution of the windows behind them.
pub const MAX_EDGE: u32 = 256;

/// One window's last appearance before it was hidden.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub width: u32,
    pub height: u32,
    /// Row-major RGBA, `width * height * 4` bytes, no padding.
    ///
    /// Unpadded deliberately: every consumer so far copies it into a buffer of
    /// its own choosing, and a stride would be a second thing to get right at
    /// each of them.
    pub pixels: Vec<u8>,
}

impl Snapshot {
    /// Whether the buffer actually holds the pixels the header claims.
    ///
    /// Checked rather than assumed at the boundary where a snapshot is handed
    /// out: a short buffer becomes an out-of-bounds read in whatever draws it,
    /// and that is a long way from the arithmetic that produced it.
    pub fn is_consistent(&self) -> bool {
        self.pixels.len() == (self.width as usize) * (self.height as usize) * 4
    }

    /// Whether every pixel is fully transparent.
    ///
    /// A capture that produces this is a capture that failed without saying so:
    /// the dimensions are right, the buffer is the right length, and the
    /// thumbnail is a hole. It is worth distinguishing because the symptom
    /// appears in whatever *draws* the thumbnail, a process and a protocol away
    /// from the render that produced nothing.
    ///
    /// Transparency rather than blackness is the test: the capture clears to
    /// transparent so a window with rounded corners keeps them, so an empty
    /// render leaves alpha at zero everywhere.
    pub fn is_blank(&self) -> bool {
        self.pixels.chunks_exact(4).all(|pixel| pixel[3] == 0)
    }
}

/// Every held snapshot, by window.
#[derive(Debug, Default)]
pub struct Stage {
    snapshots: HashMap<ToplevelId, Snapshot>,
}

impl Stage {
    pub fn get(&self, id: ToplevelId) -> Option<&Snapshot> {
        self.snapshots.get(&id)
    }

    pub fn insert(&mut self, id: ToplevelId, snapshot: Snapshot) {
        self.snapshots.insert(id, snapshot);
    }

    /// Drop a window's snapshot.
    ///
    /// Called when a window is restored and when it closes. Both matter: a
    /// snapshot kept past a restore is stale the moment the user types into the
    /// window, and one kept past a close is a leak that never ends, because
    /// nothing will ask for that id again.
    pub fn forget(&mut self, id: ToplevelId) {
        self.snapshots.remove(&id);
    }

    /// Drop everything not in the given set.
    ///
    /// The sweep exists because `forget` is a call someone can fail to make.
    /// Restoring and closing are the two paths today; a third — a window that
    /// disappears without either, which is what a client crash looks like —
    /// would otherwise hold its snapshot for the life of the session.
    pub fn retain_only(&mut self, live: &[ToplevelId]) {
        self.snapshots.retain(|id, _| live.contains(id));
    }

    pub fn len(&self) -> usize {
        self.snapshots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.snapshots.is_empty()
    }
}

/// The size a snapshot should be stored at, preserving aspect ratio.
///
/// Never upscales: a window smaller than the cap is stored as it is, because
/// enlarging it would spend memory to add no detail and the consumer can scale
/// up as well as this could.
pub fn thumbnail_size(width: u32, height: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= MAX_EDGE || longest == 0 {
        return (width, height);
    }
    // Rounded up, and floored at 1. A 4000×3 window scales to 256×1, and a
    // zero-height thumbnail is a buffer nothing can draw.
    let scale = |v: u32| ((v as u64 * MAX_EDGE as u64).div_ceil(longest as u64) as u32).max(1);
    (scale(width), scale(height))
}

/// Downscale RGBA by box-averaging, into `thumbnail_size`.
///
/// Box average rather than nearest-neighbour. Nearest is cheaper and is what a
/// first version reaches for, and on window content it is visibly wrong in a
/// specific way: text and one-pixel window borders are exactly the high
/// frequencies it drops, so a terminal thumbnails to a grey rectangle with
/// speckle where the text was. Averaging keeps the *shape* of a window, which
/// is the entire information a thumbnail carries at this size.
///
/// `None` when the input does not describe a real image, rather than a panic or
/// a partly-filled buffer: the caller is a render loop, and a frame that cannot
/// produce a thumbnail should skip it and carry on.
pub fn downscale(src: &[u8], width: u32, height: u32) -> Option<Snapshot> {
    if width == 0 || height == 0 {
        return None;
    }
    if src.len() < (width as usize) * (height as usize) * 4 {
        return None;
    }

    let (out_w, out_h) = thumbnail_size(width, height);
    if (out_w, out_h) == (width, height) {
        return Some(Snapshot {
            width,
            height,
            pixels: src[..(width as usize) * (height as usize) * 4].to_vec(),
        });
    }

    let mut pixels = vec![0u8; (out_w as usize) * (out_h as usize) * 4];
    for oy in 0..out_h {
        // The source rows this output row covers. Computed from the output
        // index in both directions rather than by stepping a fractional
        // accumulator, which drifts and leaves the last row half-sampled.
        let y0 = (oy as u64 * height as u64 / out_h as u64) as u32;
        let y1 = (((oy + 1) as u64 * height as u64).div_ceil(out_h as u64) as u32).max(y0 + 1);
        for ox in 0..out_w {
            let x0 = (ox as u64 * width as u64 / out_w as u64) as u32;
            let x1 = (((ox + 1) as u64 * width as u64).div_ceil(out_w as u64) as u32).max(x0 + 1);

            let mut sums = [0u64; 4];
            let mut count = 0u64;
            for y in y0..y1.min(height) {
                for x in x0..x1.min(width) {
                    let at = ((y as usize) * (width as usize) + x as usize) * 4;
                    for channel in 0..4 {
                        sums[channel] += src[at + channel] as u64;
                    }
                    count += 1;
                }
            }
            if count == 0 {
                continue;
            }
            let out_at = ((oy as usize) * (out_w as usize) + ox as usize) * 4;
            for channel in 0..4 {
                pixels[out_at + channel] = (sums[channel] / count) as u8;
            }
        }
    }

    Some(Snapshot { width: out_w, height: out_h, pixels })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn solid(width: u32, height: u32, colour: [u8; 4]) -> Vec<u8> {
        colour
            .iter()
            .copied()
            .cycle()
            .take((width as usize) * (height as usize) * 4)
            .collect()
    }

    #[test]
    fn a_small_window_is_stored_as_it_is() {
        // Upscaling spends memory to add no detail.
        assert_eq!(thumbnail_size(100, 80), (100, 80));
        let snapshot = downscale(&solid(100, 80, [1, 2, 3, 255]), 100, 80).expect("downscales");
        assert_eq!((snapshot.width, snapshot.height), (100, 80));
        assert!(snapshot.is_consistent());
    }

    #[test]
    fn aspect_ratio_survives_the_cap() {
        // A 16:9 window is the common case and must not come back square.
        let (w, h) = thumbnail_size(1920, 1080);
        assert_eq!(w, MAX_EDGE);
        assert!(h > 140 && h < 150, "1080/1920 * 256 is about 144, got {h}");
    }

    #[test]
    fn an_extreme_ratio_never_produces_a_zero_edge() {
        // 4000x3 scaled by 256/4000 is 0.19 pixels tall. A zero-height buffer
        // is one nothing can draw, so the floor is 1.
        let (w, h) = thumbnail_size(4000, 3);
        assert_eq!(w, MAX_EDGE);
        assert_eq!(h, 1);
        let snapshot = downscale(&solid(4000, 3, [9, 9, 9, 255]), 4000, 3).expect("downscales");
        assert!(snapshot.is_consistent());
        assert!(snapshot.height >= 1);
    }

    #[test]
    fn a_solid_image_downscales_to_the_same_colour() {
        // Box averaging over identical pixels must be exact — any drift here is
        // an arithmetic bug that would show as a thumbnail tinted differently
        // from the window it came from.
        let snapshot = downscale(&solid(1000, 1000, [40, 80, 120, 255]), 1000, 1000).unwrap();
        assert!(snapshot.is_consistent());
        for chunk in snapshot.pixels.chunks_exact(4) {
            assert_eq!(chunk, [40, 80, 120, 255]);
        }
    }

    #[test]
    fn every_output_pixel_is_written() {
        // The bug this catches: a sampling loop whose ranges leave the last row
        // or column untouched, producing a thumbnail with a black edge that
        // looks like a border the window did not have.
        let src = solid(999, 501, [255, 255, 255, 255]);
        let snapshot = downscale(&src, 999, 501).unwrap();
        assert!(
            snapshot.pixels.chunks_exact(4).all(|p| p == [255, 255, 255, 255]),
            "an output pixel was never sampled"
        );
    }

    #[test]
    fn averaging_actually_averages() {
        // Two rows, black over white, scaled to one row: the result is the mean
        // and not either input. Nearest-neighbour would return one of them,
        // which is the distinction this function exists to make.
        let mut src = vec![0u8; 2 * 512 * 4];
        for x in 0..512 {
            let at = (512 + x) * 4;
            src[at..at + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
        let snapshot = downscale(&src, 512, 2).unwrap();
        assert_eq!(snapshot.height, 1, "512x2 capped at 256 is 256x1");
        for chunk in snapshot.pixels.chunks_exact(4) {
            assert_eq!(chunk[0], 127, "expected the mean of 0 and 255");
        }
    }

    #[test]
    fn a_short_buffer_is_refused_rather_than_read_past() {
        // The caller is a render loop reading back from the GPU; a download
        // that returned less than advertised must not become an out-of-bounds
        // read in the scaler.
        assert!(downscale(&[0u8; 16], 100, 100).is_none());
        assert!(downscale(&[], 0, 0).is_none());
    }

    #[test]
    fn an_empty_render_is_recognisable_as_empty() {
        // The failure this names: correct dimensions, correct buffer length,
        // and nothing in it. Without the check it surfaces as a blank tile in
        // the dock, a process away from the cause.
        let blank = downscale(&vec![0u8; 64 * 64 * 4], 64, 64).unwrap();
        assert!(blank.is_blank());

        let drawn = downscale(&solid(64, 64, [10, 20, 30, 255]), 64, 64).unwrap();
        assert!(!drawn.is_blank());

        // A single opaque pixel is enough — the window drew *something*.
        let mut mostly_empty = vec![0u8; 64 * 64 * 4];
        mostly_empty[3] = 255;
        assert!(!downscale(&mostly_empty, 64, 64).unwrap().is_blank());
    }

    #[test]
    fn snapshots_are_dropped_on_restore_and_on_close() {
        let mut stage = Stage::default();
        let snapshot = downscale(&solid(8, 8, [1, 1, 1, 255]), 8, 8).unwrap();
        stage.insert(ToplevelId(1), snapshot.clone());
        stage.insert(ToplevelId(2), snapshot);
        assert_eq!(stage.len(), 2);

        stage.forget(ToplevelId(1));
        assert!(stage.get(ToplevelId(1)).is_none());
        assert!(stage.get(ToplevelId(2)).is_some());

        // The sweep is the backstop for a window that vanished without either
        // path being taken — what a client crash looks like from here.
        stage.retain_only(&[]);
        assert!(stage.is_empty(), "the sweep must drop what nothing claims");
    }

    #[test]
    fn a_consistent_snapshot_is_distinguishable_from_a_broken_one() {
        let mut snapshot = downscale(&solid(4, 4, [1, 2, 3, 4]), 4, 4).unwrap();
        assert!(snapshot.is_consistent());
        snapshot.pixels.truncate(3);
        assert!(!snapshot.is_consistent());
    }
}
