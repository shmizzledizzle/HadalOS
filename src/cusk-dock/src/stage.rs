//! Pictures of minimised windows, as the dock holds them.
//!
//! The client half of `hadal_stage_v1`. The protocol objects live on the event
//! thread in `windows.rs`, alongside the toplevel handles they refer to, so
//! this module is not a second connection — it is the shared store the UI
//! reads and the two small pieces of arithmetic between a file descriptor and
//! something iced will draw.
//!
//! Those two pieces are here rather than in `windows.rs` because they are the
//! parts that can be wrong in a way a test can catch. Reading a descriptor
//! cannot be tested without a compositor; deciding that 147456 bytes is not a
//! 256x144 image can.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

/// A thumbnail, in the layout iced wants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Thumbnail {
    pub width: u32,
    pub height: u32,
    /// Straight-alpha RGBA, one byte per channel.
    pub pixels: Vec<u8>,
    /// Bumped whenever the pixels change, and the only thing the UI compares.
    ///
    /// The UI caches an `image::Handle` per window and has to know, once a
    /// frame, whether the cached one is still right. Answering that by
    /// comparing the pixels means reading a quarter of a megabyte per
    /// minimised window per frame — and reading all of it, because the answer
    /// is almost always "equal" and equality has no early exit. With five
    /// windows minimised that is tens of megabytes a second of memcmp to
    /// establish that nothing happened.
    ///
    /// Assigned by the event thread, which is the only writer and therefore
    /// the only place that can say whether something changed.
    pub revision: u64,
}

/// What the UI reads, keyed by the window id `windows.rs` hands out.
///
/// Separate from the window list rather than a field on `Window`, because the
/// two change on completely different schedules: the list is republished on
/// every title change, and cloning a quarter-megabyte of pixels per keystroke
/// in somebody's editor is not a thing to do by accident.
pub type Thumbs = Arc<Mutex<HashMap<u32, Thumbnail>>>;

/// Read `height * stride` bytes out of a descriptor the compositor sealed.
///
/// Copied out rather than kept mapped. A mapping would avoid the copy and
/// would mean the dock holding a file descriptor per minimised window for as
/// long as it draws it — and iced needs owned pixels to build an image handle
/// from anyway, so the copy happens either way. At a 256-pixel long edge this
/// is at most 256 kB, once per minimise.
///
/// # Safety
///
/// The mapping is `PROT_READ` and `MAP_PRIVATE`, and the compositor sealed the
/// file against shrinking before sending it — which is the part that matters.
/// A file that could shrink under a live mapping turns a read of the last row
/// into `SIGBUS`, and a signal is not something the caller can handle.
/// `hadal_stage_v1` requires the seal for exactly this reason.
pub fn read(fd: std::os::fd::BorrowedFd<'_>, width: u32, height: u32, stride: u32) -> Option<Thumbnail> {
    use rustix::mm::{mmap, munmap, MapFlags, ProtFlags};

    let len = expected_len(width, height, stride)?;

    // The file must be at least as long as the geometry claims. Trusting the
    // event and mapping past the end is the SIGBUS above by another route —
    // through a compositor bug rather than a malicious one, which is the more
    // likely of the two.
    let actual = rustix::fs::fstat(&fd).ok()?.st_size;
    if (actual as u64) < len as u64 {
        return None;
    }

    // SAFETY: `len` is non-zero and the file is at least that long, checked
    // above. The mapping is read-only and private, so nothing else can observe
    // writes through it, and it is unmapped before this function returns.
    let map = unsafe {
        mmap(
            std::ptr::null_mut(),
            len,
            ProtFlags::READ,
            MapFlags::PRIVATE,
            &fd,
            0,
        )
        .ok()?
    };

    // SAFETY: `map` is a valid mapping of `len` bytes, just returned by mmap.
    let bytes = unsafe { std::slice::from_raw_parts(map as *const u8, len) };
    let thumbnail = unpack(bytes, width, height, stride);

    // SAFETY: unmapping exactly what was mapped. `bytes` is not used after
    // this point, and `unpack` copied what it needed.
    unsafe {
        let _ = munmap(map, len);
    }

    thumbnail
}

/// How many bytes an image of this shape occupies, or `None` if it cannot.
///
/// Checked arithmetic throughout. These three numbers come off the wire, and
/// `width * height * 4` in `u32` overflows at a little over 32 megapixels —
/// which a thumbnail never is, so the guard is not about plausible values. It
/// is about the implausible ones producing a small number instead of an error,
/// and a small number here becomes a short allocation and an out-of-bounds
/// read.
fn expected_len(width: u32, height: u32, stride: u32) -> Option<usize> {
    if width == 0 || height == 0 {
        return None;
    }
    // A stride that cannot hold a row means the rest of the arithmetic is
    // describing something other than this image.
    if stride < width.checked_mul(4)? {
        return None;
    }
    let len = (stride as usize).checked_mul(height as usize)?;
    (len > 0).then_some(len)
}

/// Turn mapped bytes into straight-alpha RGBA, dropping any row padding.
///
/// Two conversions, both of which have to happen somewhere and both of which
/// belong on this side:
///
/// - **Stride to packed.** The protocol carries a stride because a compositor
///   is entitled to pad its rows; iced wants `width * height * 4` with nothing
///   between the rows. Ignoring the distinction gives an image that shears
///   progressively down its height — recognisable once you have seen it, and
///   mystifying the first time.
///
/// - **Premultiplied to straight.** Wayland composites in premultiplied alpha
///   and the compositor renders in it, so the bytes arrive that way.
///   `iced::widget::image` expects straight alpha. Passing premultiplied
///   pixels through unchanged makes translucent edges — a rounded corner, a
///   window's own shadow — read as darkened rather than transparent, which
///   looks like a wrong colour rather than a wrong format.
fn unpack(bytes: &[u8], width: u32, height: u32, stride: u32) -> Option<Thumbnail> {
    let (w, h, s) = (width as usize, height as usize, stride as usize);
    if bytes.len() < s.checked_mul(h)? {
        return None;
    }

    let mut pixels = Vec::with_capacity(w * h * 4);
    for row in 0..h {
        let start = row * s;
        for px in bytes[start..start + w * 4].chunks_exact(4) {
            let a = px[3];
            match a {
                // Fully transparent premultiplied pixels carry no colour at
                // all — dividing by zero alpha has no answer, and the answer
                // does not matter because nothing is drawn.
                0 => pixels.extend_from_slice(&[0, 0, 0, 0]),
                255 => pixels.extend_from_slice(px),
                a => {
                    // Rounded, not truncated. Truncating darkens every
                    // translucent pixel by up to one level, which across a
                    // gradient is a visible band.
                    let un = |c: u8| (((c as u32 * 255) + (a as u32 / 2)) / a as u32).min(255) as u8;
                    pixels.extend_from_slice(&[un(px[0]), un(px[1]), un(px[2]), a]);
                }
            }
        }
    }

    Some(Thumbnail {
        width,
        height,
        pixels,
        // Stamped by the caller, which is the only thing that knows how many
        // pictures of this window have come before.
        revision: 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zero_dimension_has_no_length() {
        assert_eq!(expected_len(0, 10, 40), None);
        assert_eq!(expected_len(10, 0, 40), None);
    }

    #[test]
    fn a_stride_too_short_for_a_row_is_refused() {
        // 10 pixels need 40 bytes. 36 describes something else.
        assert_eq!(expected_len(10, 10, 36), None);
        assert_eq!(expected_len(10, 10, 40), Some(400));
    }

    #[test]
    fn padding_is_allowed() {
        assert_eq!(expected_len(10, 10, 48), Some(480));
    }

    #[test]
    fn implausible_dimensions_do_not_wrap_to_a_small_length() {
        // The failure this guards: in wrapping arithmetic this pair produces a
        // tiny length, which becomes a short allocation and a read past its
        // end. It must be an error instead.
        assert_eq!(expected_len(u32::MAX, u32::MAX, u32::MAX), None);
    }

    #[test]
    fn padding_is_dropped_and_rows_do_not_shear() {
        // Two rows of two opaque pixels, with four bytes of padding after
        // each. If the stride were ignored, row 1 would start inside row 0's
        // padding and every row after would slide further.
        let bytes = vec![
            1, 1, 1, 255, 2, 2, 2, 255, 0, 0, 0, 0, //
            3, 3, 3, 255, 4, 4, 4, 255, 0, 0, 0, 0,
        ];
        let out = unpack(&bytes, 2, 2, 12).expect("well formed");
        assert_eq!(out.pixels.len(), 2 * 2 * 4);
        assert_eq!(&out.pixels[0..4], &[1, 1, 1, 255]);
        assert_eq!(&out.pixels[8..12], &[3, 3, 3, 255]);
    }

    #[test]
    fn opaque_pixels_are_passed_through_untouched() {
        let bytes = vec![10, 20, 30, 255];
        let out = unpack(&bytes, 1, 1, 4).expect("well formed");
        assert_eq!(out.pixels, vec![10, 20, 30, 255]);
    }

    #[test]
    fn half_transparent_white_comes_back_white() {
        // Premultiplied 50% white is (128, 128, 128, 128). Read as straight
        // alpha it would be mid grey — the exact symptom the conversion
        // exists to prevent.
        let bytes = vec![128, 128, 128, 128];
        let out = unpack(&bytes, 1, 1, 4).expect("well formed");
        assert_eq!(out.pixels[3], 128);
        for channel in &out.pixels[0..3] {
            assert!(*channel >= 254, "expected white, got {channel}");
        }
    }

    #[test]
    fn fully_transparent_pixels_do_not_divide_by_zero() {
        let bytes = vec![0, 0, 0, 0];
        let out = unpack(&bytes, 1, 1, 4).expect("well formed");
        assert_eq!(out.pixels, vec![0, 0, 0, 0]);
    }

    #[test]
    fn unpremultiplying_never_exceeds_full_brightness() {
        // A malformed pixel whose colour is brighter than its alpha allows.
        // The division would give 510; the result must still be a byte.
        let bytes = vec![255, 255, 255, 128];
        let out = unpack(&bytes, 1, 1, 4).expect("well formed");
        assert_eq!(&out.pixels[0..3], &[255, 255, 255]);
    }

    #[test]
    fn a_short_buffer_is_refused_rather_than_read_past() {
        let bytes = vec![0u8; 4];
        assert_eq!(unpack(&bytes, 2, 2, 8), None);
    }
}
