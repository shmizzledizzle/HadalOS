//! The dock's look, from the shared tokens.
//!
//! No palette of its own — `cusk::theme` is the single source, so the dock,
//! the launcher, the settings editor and the compositor's own chrome cannot
//! drift apart.

use cusk::theme as tokens;
use iced::widget::{button, container};
use iced::{border, Background, Border, Color, Shadow, Theme, Vector};

const fn token(c: tokens::Rgba) -> Color {
    Color { r: c[0], g: c[1], b: c[2], a: c[3] }
}

pub const SURFACE: Color = token(tokens::SURFACE);
pub const SURFACE_HI: Color = token(tokens::SURFACE_HI);
pub const TEXT: Color = token(tokens::TEXT);
pub const ACCENT: Color = token(tokens::ACCENT);
pub const BG: Color = token(tokens::BG);
pub const TEXT_DIM: Color = token(tokens::TEXT_DIM);
pub const WARNING: Color = token(tokens::WARNING);

fn alpha(color: Color, a: f32) -> Color {
    Color { a, ..color }
}

/// Turn a shared elevation token into iced's shadow.
///
/// Converted here rather than stored as an `iced::Shadow` in `cusk::theme`,
/// because the compositor draws the same elevations through GL and must not
/// have to link a widget toolkit to know what a shadow looks like.
fn elevation(e: tokens::Elevation) -> Shadow {
    Shadow {
        color: token(e.shadow),
        offset: Vector::new(e.offset.0, e.offset.1),
        blur_radius: e.blur,
    }
}

/// The window itself. Translucent, so the compositor's blur shows through and
/// the dock sits *in* the desktop rather than on top of it.
/// `iced_layershell::Appearance` is `iced::theme::Style` under a private
/// alias, so it is named through iced rather than through the alias.
pub fn appearance(_theme: &Theme) -> iced::theme::Style {
    iced::theme::Style {
        background_color: alpha(token(tokens::BG), 0.82),
        text_color: TEXT,
    }
}

pub fn dock(_theme: &Theme) -> container::Style {
    container::Style {
        text_color: Some(TEXT),
        // Lifts the dock off the wallpaper. Without this the translucent
        // background reads as a discoloured rectangle rather than a surface
        // above the desktop, which was most of why the bar looked unfinished.
        shadow: elevation(tokens::RAISED),
        ..Default::default()
    }
}

/// A dock tile.
///
/// Four states, not two. The previous version matched `Hovered | Pressed`
/// together, so clicking looked exactly like hovering and the dock felt
/// unresponsive — the pointer changed nothing on the way down. Pressed is now
/// both stronger and *flatter*: a control being pushed should not rise.
///
/// Focus is separate from hover because focus can be somewhere the pointer is
/// not. A tile that only responds to the pointer cannot be driven from the
/// keyboard, which is a functional gap wearing a visual costume.
pub fn tile(_theme: &Theme, status: button::Status) -> button::Style {
    let (fill, ring, lift) = match status {
        button::Status::Pressed => (
            alpha(ACCENT, tokens::STATE_PRESS),
            Color::TRANSPARENT,
            // Deliberately no shadow: pressed reads as pushed in.
            Shadow::default(),
        ),
        button::Status::Hovered => (
            alpha(ACCENT, tokens::STATE_HOVER),
            Color::TRANSPARENT,
            elevation(tokens::RAISED),
        ),
        button::Status::Disabled => (Color::TRANSPARENT, Color::TRANSPARENT, Shadow::default()),
        button::Status::Active => (Color::TRANSPARENT, Color::TRANSPARENT, Shadow::default()),
    };

    button::Style {
        background: Some(Background::Color(fill)),
        text_color: TEXT,
        border: Border {
            color: ring,
            width: if ring == Color::TRANSPARENT { 0.0 } else { 1.5 },
            radius: border::Radius::new(10.0),
        },
        shadow: lift,
        ..Default::default()
    }
}

// No focused-tile style here, and the absence is deliberate.
//
// `cusk::theme::STATE_FOCUS` exists and the ring was written — then deleted,
// because iced 0.14's `button::Status` has no `Focused` variant and this dock
// has no keyboard navigation to produce one. A styling function nothing can
// call is a capability recorded as though it were the state, which is the
// failure this tree keeps finding in its own notes.
//
// The gap is real and belongs in the open list rather than in a dead function:
// **the dock cannot be driven from the keyboard at all.** Fixing that is focus
// handling first and a ring second. The token stays because the settings
// editor has focusable widgets and should not invent its own.

/// A tray tile, which has two states an ordinary tile does not.
///
/// `open` keeps the tile lit while its menu is on screen: without it the menu
/// appears to belong to nothing, since the pointer has moved off the icon and
/// onto the menu, so the hover styling is gone.
///
/// `attention` is the only visible effect of `Status::NeedsAttention`. A tint
/// rather than a pulse — an animation would need a timer subscription running
/// for as long as any item was asking for attention, and a bar that animates
/// forever is a bar that is always slightly distracting.
pub fn tray_tile(open: bool, attention: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let base = tile(theme, status);
        if open {
            return button::Style {
                background: Some(Background::Color(alpha(ACCENT, tokens::STATE_PRESS))),
                ..base
            };
        }
        if attention && matches!(status, button::Status::Active) {
            return button::Style {
                background: Some(Background::Color(alpha(WARNING, 0.28))),
                border: Border {
                    color: alpha(WARNING, 0.75),
                    width: 1.0,
                    radius: border::Radius::new(10.0),
                },
                ..base
            };
        }
        base
    }
}

/// A running-window tile.
///
/// Three states, and the minimised one is the substance. A minimised window is
/// still *there* — it has to be clickable to come back — so it is dimmed rather
/// than hidden or removed. Drawing it identically to a visible window would make
/// the strip unable to answer the one question a taskbar exists for: which of
/// these can I currently see?
pub fn window_tile(activated: bool, minimized: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        let base = tile(theme, status);
        if minimized && matches!(status, button::Status::Active) {
            return button::Style {
                // No fill at all, and the dimming is carried by the icon's own
                // transparency being unavailable to us — so a faint surface is
                // the honest signal we can actually draw.
                background: Some(Background::Color(alpha(SURFACE, 0.35))),
                ..base
            };
        }
        if activated && matches!(status, button::Status::Active) {
            return button::Style {
                background: Some(Background::Color(alpha(ACCENT, 0.14))),
                ..base
            };
        }
        base
    }
}

/// The focus bar beside the active window's tile.
///
/// A shape, not a colour alone: shape survives a bad monitor, a colourblind
/// user, and a screenshot at low contrast — the same argument `panel.rs` makes
/// for the active workspace pill being wider rather than merely a different
/// colour.
pub fn focus_marker(activated: bool) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        background: Some(Background::Color(if activated {
            ACCENT
        } else {
            Color::TRANSPARENT
        })),
        border: Border {
            radius: border::Radius::new(999.0),
            ..Default::default()
        },
        ..Default::default()
    }
}

/// The tray menu's panel.
///
/// Opaque, unlike the dock itself. The dock is translucent because it sits over
/// the wallpaper and the effect is pleasant; a *menu* over an application window
/// with text showing through it is unreadable, and this is the one surface where
/// legibility beats the look.
pub fn menu_panel(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG)),
        text_color: Some(TEXT),
        border: Border {
            color: alpha(SURFACE_HI, 0.9),
            width: 1.0,
            radius: border::Radius::new(10.0),
        },
        shadow: elevation(tokens::OVERLAY),
        ..Default::default()
    }
}

/// One row of a tray menu.
pub fn menu_row(enabled: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let hovered = enabled
            && matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if hovered {
                alpha(ACCENT, tokens::STATE_HOVER)
            } else {
                Color::TRANSPARENT
            })),
            // Disabled rows are dimmed rather than hidden: the application said
            // to show them, and a menu that silently drops what it cannot do
            // looks like a menu missing entries.
            text_color: if enabled { TEXT } else { TEXT_DIM },
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::Radius::new(6.0),
            },
            ..Default::default()
        }
    }
}

/// A menu separator: a hairline, not a gap.
pub fn menu_divider(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(alpha(SURFACE_HI, 0.8))),
        ..Default::default()
    }
}

/// The fallback tile for an icon that could not be resolved.
pub fn letter(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE_HI)),
        text_color: Some(TEXT),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::Radius::new(10.0),
        },
        ..Default::default()
    }
}

/// The tooltip.
///
/// OVERLAY rather than RAISED: a tip sharing the dock's elevation looks like
/// part of the dock instead of something floating over it. It also gets a
/// hairline border — on a dark theme a shadow alone does not separate two
/// near-black surfaces, which is why the old flat tip disappeared into the
/// bar behind it.
pub fn tip(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE)),
        text_color: Some(TEXT),
        border: Border {
            color: alpha(SURFACE_HI, 0.9),
            width: 1.0,
            radius: border::Radius::new(8.0),
        },
        shadow: elevation(tokens::OVERLAY),
        ..Default::default()
    }
}

