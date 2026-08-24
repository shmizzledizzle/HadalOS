//! The launcher's look, from the shared tokens.
//!
//! No palette of its own — `cusk::theme` is the single source, so the launcher,
//! the settings editor and the compositor's focus ring cannot disagree.

use cusk::theme as tokens;
use iced::widget::{button, container, scrollable, text_input};
use iced::{border, Background, Border, Color, Theme};

const fn token(c: tokens::Rgba) -> Color {
    Color { r: c[0], g: c[1], b: c[2], a: c[3] }
}

pub const BG: Color = token(tokens::BG);
pub const SURFACE: Color = token(tokens::SURFACE);
pub const SURFACE_HI: Color = token(tokens::SURFACE_HI);
pub const INSET: Color = token(tokens::INSET);
pub const TEXT: Color = token(tokens::TEXT);
pub const TEXT_DIM: Color = token(tokens::TEXT_DIM);
pub const ACCENT: Color = token(tokens::ACCENT);

const RADIUS: f32 = 14.0;
const RADIUS_CONTROL: f32 = 10.0;

fn alpha(color: Color, a: f32) -> Color {
    Color { a, ..color }
}


pub fn window(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG)),
        text_color: Some(TEXT),
        // The launcher draws its own rounding because it asks for no
        // decorations — and because at this size the compositor's corner
        // radius would clip a window that is mostly padding.
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::Radius::new(RADIUS),
        },
        ..Default::default()
    }
}

pub fn field(_theme: &Theme, status: text_input::Status) -> text_input::Style {
    let active = !matches!(status, text_input::Status::Active);
    text_input::Style {
        background: Background::Color(INSET),
        border: Border {
            color: if active { alpha(ACCENT, 0.55) } else { Color::TRANSPARENT },
            width: if active { 1.0 } else { 0.0 },
            radius: border::Radius::new(RADIUS_CONTROL),
        },
        icon: TEXT_DIM,
        placeholder: TEXT_DIM,
        value: TEXT,
        selection: alpha(ACCENT, 0.35),
    }
}

/// The selection bar. One accent shape rather than a box around the row, which
/// is how both reference shells mark a choice.
pub fn marker(selected: bool) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        background: Some(Background::Color(if selected {
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

/// A menu row: an application in the right pane, or a category in the sidebar.
///
/// A button rather than the plain container the flat list used, because a
/// container has no `on_press` — the old list could only be driven by Enter, so
/// clicking an application did nothing at all. Transparent until hovered or
/// selected, so the menu reads as text with one marked line rather than as a
/// grid of tiles.
pub fn menu_row(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(match (selected, hovered) {
                (true, _) => SURFACE,
                // Distinctly weaker than the selection. Matching them would
                // make the keyboard's idea of "current" and the pointer's
                // indistinguishable, and Enter launches only one of them.
                (false, true) => alpha(SURFACE, 0.5),
                (false, false) => Color::TRANSPARENT,
            })),
            text_color: TEXT,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::Radius::new(RADIUS_CONTROL),
            },
            ..Default::default()
        }
    }
}

/// A sidebar category.
///
/// The accent is carried by the text rather than by a filled pill: the sidebar
/// is a list of nine or ten words, and ten filled shapes stacked vertically
/// compete with the applications beside them for the eye.
pub fn category(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let hovered = matches!(status, button::Status::Hovered | button::Status::Pressed);
        button::Style {
            background: Some(Background::Color(if selected {
                alpha(ACCENT, 0.16)
            } else if hovered {
                alpha(SURFACE, 0.5)
            } else {
                Color::TRANSPARENT
            })),
            text_color: if selected { ACCENT } else { TEXT_DIM },
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::Radius::new(RADIUS_CONTROL),
            },
            ..Default::default()
        }
    }
}

/// The hairline between the sidebar and the application pane.
pub fn divider(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(alpha(SURFACE_HI, 0.55))),
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
