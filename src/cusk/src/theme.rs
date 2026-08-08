//! The palette, shared by the compositor and the settings editor.
//!
//! Structure from the niri and KaOS references — no borders, large radii, low
//! contrast, one accent doing all the emphasis. **Colour from HadalOS's own
//! artwork**: the launcher icon is a deep-ocean gradient over trench rock with
//! a single cyan glow at the base, and a desktop whose accent disagrees with
//! its own icon looks like two projects.
//!
//! Sampled from `HadalOS_Graphics/Icons/menu_icon.png`: gradient `#0C5BA8` to
//! `#041E5C`, rock `#0D1F22`, and the glow `#11C1C6` — which becomes the
//! accent, because in the icon it is the only bright thing in a very dark
//! image, which is exactly the job an accent does.
//!
//! It lives in the library rather than in either binary because both draw the
//! same design:
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

/// Window background. Trench-dark: near-black with the icon's blue in it,
/// rather than the neutral black a terminal uses.
pub const BG: Rgba = rgb(0x08, 0x11, 0x1A);
/// Cards and controls sitting on the background.
pub const SURFACE: Rgba = rgb(0x0F, 0x1C, 0x2B);
/// Hover and selected states.
pub const SURFACE_HI: Rgba = rgb(0x18, 0x2A, 0x3E);
/// Recessed fills — pickers, menus, inputs.
pub const INSET: Rgba = rgb(0x0A, 0x16, 0x22);
pub const TEXT: Rgba = rgb(0xDF, 0xEB, 0xF2);
/// Descriptions and units. Muted enough to recede, and deliberately not
/// raised: the references keep secondary text close to its background.
pub const TEXT_DIM: Rgba = rgb(0x84, 0x9C, 0xB0);
/// The glow at the base of the launcher icon. One accent carries every
/// piece of emphasis; a second colour immediately reads as a different design.
pub const ACCENT: Rgba = rgb(0x11, 0xC1, 0xC6);
pub const DANGER: Rgba = rgb(0xE8, 0x7A, 0x8E);
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
