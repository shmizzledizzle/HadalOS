//! The palette, shared by the compositor and the settings editor.
//!
//! Sampled from the reference screenshots — see `docs/cusk.md` §6. It lives in
//! the library rather than in either binary because both draw the same design:
//! the editor styles its widgets from these, and the compositor draws window
//! chrome from them. A second copy would drift, and the drift would show up as
//! a focus ring that does not match the accent in the settings window that sets
//! it.
//!
//! Values are linear-ish sRGB components in 0..1, which is what both a GL
//! renderer and iced want.

/// Red, green, blue, alpha, each 0..1.
pub type Rgba = [f32; 4];

const fn rgb(r: u8, g: u8, b: u8) -> Rgba {
    [r as f32 / 255.0, g as f32 / 255.0, b as f32 / 255.0, 1.0]
}

/// Window background. Near-black with a purple cast.
pub const BG: Rgba = rgb(0x17, 0x17, 0x1F);
/// Cards and controls sitting on the background.
pub const SURFACE: Rgba = rgb(0x23, 0x23, 0x31);
/// Hover and selected states.
pub const SURFACE_HI: Rgba = rgb(0x2E, 0x2E, 0x40);
/// Recessed fills — pickers, menus, inputs.
pub const INSET: Rgba = rgb(0x1E, 0x1E, 0x2A);
pub const TEXT: Rgba = rgb(0xE6, 0xE5, 0xF2);
pub const TEXT_DIM: Rgba = rgb(0x9B, 0x9A, 0xB8);
/// Periwinkle, between niri's `#A3C9FD` and KaOS's `#8189B9`. One accent
/// carries every piece of emphasis in the references.
pub const ACCENT: Rgba = rgb(0xA3, 0xB4, 0xE8);
pub const DANGER: Rgba = rgb(0xE8, 0x89, 0x9B);
pub const WARNING: Rgba = rgb(0xE8, 0xC0, 0x8A);

/// The ring around an unfocused window. Dim enough to read as absence rather
/// than as a second kind of emphasis.
pub const RING_IDLE: Rgba = [
    SURFACE_HI[0],
    SURFACE_HI[1],
    SURFACE_HI[2],
    0.9,
];

/// Premultiply by alpha, which is what the GL renderer blends with.
pub const fn premultiplied(c: Rgba) -> Rgba {
    [c[0] * c[3], c[1] * c[3], c[2] * c[3], c[3]]
}
