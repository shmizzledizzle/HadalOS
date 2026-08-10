//! The dock's look, from the shared tokens.
//!
//! No palette of its own — `cusk::theme` is the single source, so the dock,
//! the launcher, the settings editor and the compositor's own chrome cannot
//! drift apart.

use cusk::theme as tokens;
use iced::widget::{button, container};
use iced::{border, Background, Border, Color, Theme};

const fn token(c: tokens::Rgba) -> Color {
    Color { r: c[0], g: c[1], b: c[2], a: c[3] }
}

pub const SURFACE: Color = token(tokens::SURFACE);
pub const SURFACE_HI: Color = token(tokens::SURFACE_HI);
pub const TEXT: Color = token(tokens::TEXT);
pub const ACCENT: Color = token(tokens::ACCENT);

fn alpha(color: Color, a: f32) -> Color {
    Color { a, ..color }
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
        ..Default::default()
    }
}

pub fn tile(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
    button::Style {
        background: Some(Background::Color(if hovered {
            alpha(ACCENT, 0.22)
        } else {
            Color::TRANSPARENT
        })),
        text_color: TEXT,
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::Radius::new(10.0),
        },
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

pub fn tip(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE)),
        text_color: Some(TEXT),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::Radius::new(8.0),
        },
        ..Default::default()
    }
}

