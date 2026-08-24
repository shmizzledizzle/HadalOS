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

// ── Elevation ──────────────────────────────────────────────────────────
//
// Added 2026-08-24, because the dock was flat against the wallpaper and read
// as rough next to XFCE. A translucent panel with no shadow does not look like
// it is *above* anything; it looks like a discoloured rectangle.
//
// Structure taken from Plasma's Breeze, which uses a short fixed set of
// elevation steps rather than a per-widget shadow — a set small enough that
// two surfaces at the same height always match. Hyprland's shadow parameters
// (large blur, small offset, very low alpha) informed the values.
//
// **Approaches only, no code.** cusk is `GPL-2.0-only`. Plasma is GPL-2+ and
// Hyprland is BSD-3-Clause, so code from either *could* be incorporated with
// attribution — but nothing here is copied, because these are numbers chosen
// against this palette. niri is GPL-3.0 and code from it could **not** be used
// here at all, which is worth writing down since niri is the closest reference
// in stack terms and the temptation is real.
//
// The shadow is near-black rather than a darkened accent: on a background this
// dark, a tinted shadow reads as a coloured halo instead of depth.

/// What a surface at a given height casts. Renderer-agnostic on purpose — the
/// compositor's GL path and iced both consume these.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Elevation {
    pub shadow: Rgba,
    /// x, y in logical pixels. Positive y is downward in both renderers.
    pub offset: (f32, f32),
    pub blur: f32,
}

/// Surfaces resting on the desktop: the dock, the panel.
///
/// Deliberately shallow. A heavy shadow on a full-width bar reads as a drop
/// shadow on the whole screen rather than as depth.
pub const RAISED: Elevation = Elevation {
    shadow: [0.0, 0.0, 0.0, 0.34],
    offset: (0.0, 2.0),
    blur: 12.0,
};

/// Surfaces floating above everything: tooltips, menus, the launcher.
///
/// Deeper than RAISED, because an overlay that shares the dock's elevation
/// looks like part of the dock.
pub const OVERLAY: Elevation = Elevation {
    shadow: [0.0, 0.0, 0.0, 0.45],
    offset: (0.0, 6.0),
    blur: 24.0,
};

// ── Interaction states ─────────────────────────────────────────────────
//
// The dock previously drew Hovered and Pressed identically, so a click gave no
// feedback distinct from the pointer merely being there. Every reference
// distinguishes them, and the distinction is what makes a control feel
// responsive rather than laggy.

/// Accent alpha for a hovered control.
pub const STATE_HOVER: f32 = 0.22;
/// Accent alpha for a pressed one. Stronger *and* the caller should drop the
/// elevation — a button that is being pushed should not also be rising.
pub const STATE_PRESS: f32 = 0.38;
/// The keyboard focus ring. Distinct from hover because focus can be somewhere
/// the pointer is not, and a control that shows only hover is unusable from
/// the keyboard.
pub const STATE_FOCUS: f32 = 0.55;
