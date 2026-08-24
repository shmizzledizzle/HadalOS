//! The shortcut list's look, from the shared tokens.
//!
//! No palette of its own — `cusk::theme` is the single source, so this, the
//! launcher, the dock and the compositor's focus ring cannot disagree.

use cusk::theme as tokens;
use iced::widget::{container, scrollable};
use iced::{border, Background, Border, Color, Theme};

const fn token(c: tokens::Rgba) -> Color {
    Color { r: c[0], g: c[1], b: c[2], a: c[3] }
}

pub const BG: Color = token(tokens::BG);
pub const SURFACE_HI: Color = token(tokens::SURFACE_HI);
pub const TEXT: Color = token(tokens::TEXT);
pub const TEXT_DIM: Color = token(tokens::TEXT_DIM);
pub const ACCENT: Color = token(tokens::ACCENT);

const RADIUS: f32 = 14.0;

fn alpha(color: Color, a: f32) -> Color {
    Color { a, ..color }
}

/// The panel itself.
///
/// Rounded here rather than by the compositor, for the reason the launcher's is:
/// the panel asks for no decorations, and at this size the compositor's corner
/// radius would clip a window that is mostly padding.
pub fn window(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG)),
        text_color: Some(TEXT),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::Radius::new(RADIUS),
        },
        ..Default::default()
    }
}

pub fn scroller(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let base = scrollable::default(theme, status);
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(alpha(SURFACE_HI, 0.9)),
            border: Border {
                radius: border::Radius::new(999.0),
                ..Default::default()
            },
        },
    };
    scrollable::Style {
        container: container::Style::default(),
        vertical_rail: rail,
        horizontal_rail: rail,
        ..base
    }
}
