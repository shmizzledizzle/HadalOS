//! The look, in one file.
//!
//! Sampled from the reference screenshots rather than guessed at. niri's own
//! panels and KaOS's dark shell share a language: a desaturated **blue-purple
//! slate** (KaOS's panel measures `#303243`, its deepest surface `#1D1D2D`), a
//! periwinkle accent (niri `#A3C9FD`, KaOS `#8189B9`), and — the part that is
//! easy to miss — **no borders anywhere**. Both separate surfaces purely by
//! fill lightness. A hairline around a rounded card is what makes a design
//! read as a toolkit dialog instead of a shell.
//!
//! Contrast is deliberately low. The references keep secondary text close to
//! its background, and raising it "for legibility" is the single change that
//! would most make this stop looking like the thing it is copying.
//!
//! Everything visual lives here on purpose. The compositor's own chrome —
//! focus rings, borders, corner radii — is meant to adopt the same tokens
//! later, and a palette scattered through view code cannot be adopted by
//! anything. Retuning the aesthetic should be editing this file, not hunting
//! literals through a UI tree.

use cusk::theme as tokens;
use iced::border;
use iced::widget::{button, container, pick_list, scrollable, slider, toggler};
use iced::{Background, Border, Color, Shadow, Theme, Vector};

// ── palette ──────────────────────────────────────────────────────────────

/// Window background. Near-black with a purple cast — flat black reads as a
/// terminal, and neutral grey reads as a stock toolkit.
pub const BG: Color = token(tokens::BG);
/// Cards and controls sitting on the background.
pub const SURFACE: Color = token(tokens::SURFACE);
/// Hover and selected states.
pub const SURFACE_HI: Color = token(tokens::SURFACE_HI);
/// Recessed fills — pickers, menus, anything that should read as *into* the
/// surface rather than on top of it. Darker than the card, which is how both
/// references indicate an input.
pub const INSET: Color = token(tokens::INSET);
pub const TEXT: Color = token(tokens::TEXT);
/// Descriptions and units. Muted enough to recede, light enough to read.
pub const TEXT_DIM: Color = token(tokens::TEXT_DIM);
/// Periwinkle, between niri's `#A3C9FD` and KaOS's `#8189B9`. One accent
/// carries every piece of emphasis in both references; a second colour would
/// immediately read as a different design.
pub const ACCENT: Color = token(tokens::ACCENT);
pub const DANGER: Color = token(tokens::DANGER);
pub const WARNING: Color = token(tokens::WARNING);

/// Shared with the compositor, which draws window chrome from the same
/// numbers. A private copy here would drift, and the drift would show as a
/// focus ring that does not match the accent in the window that sets it.
const fn token(c: tokens::Rgba) -> Color {
    Color { r: c[0], g: c[1], b: c[2], a: c[3] }
}

fn alpha(color: Color, a: f32) -> Color {
    Color { a, ..color }
}

// ── metrics ──────────────────────────────────────────────────────────────

/// Card corners. Large enough to be the defining feature rather than a
/// softened edge — this is the single most recognisable thing about the look.
pub const RADIUS_CARD: f32 = 16.0;
pub const RADIUS_CONTROL: f32 = 12.0;
/// Fully round, for pills and slider handles.
pub const RADIUS_PILL: f32 = 999.0;
pub const GAP: f32 = 10.0;
pub const PAD: f32 = 18.0;

pub fn theme() -> Theme {
    Theme::custom(
        "cusk".to_string(),
        iced::theme::Palette {
            background: BG,
            text: TEXT,
            primary: ACCENT,
            success: ACCENT,
            warning: WARNING,
            danger: DANGER,
        },
    )
}

// ── containers ───────────────────────────────────────────────────────────

pub fn window(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(BG)),
        text_color: Some(TEXT),
        ..Default::default()
    }
}

/// One setting, on its own card.
pub fn card(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(SURFACE)),
        text_color: Some(TEXT),
        // No border. Both references separate surfaces by fill alone, and a
        // hairline here is the difference between a shell and a dialog box.
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::Radius::new(RADIUS_CARD),
        },
        // A heavy shadow on a dark theme turns into a smear; this only has to
        // lift the card off the background by a hair.
        shadow: Shadow {
            color: alpha(Color::BLACK, 0.25),
            offset: Vector::new(0.0, 1.0),
            blur_radius: 8.0,
        },
        ..Default::default()
    }
}

/// The active tab's underline. The whole tab indicator, deliberately — both
/// references mark selection with one thin accent line or a filled pill, never
/// with a box.
pub fn tab_underline(active: bool) -> impl Fn(&Theme) -> container::Style {
    move |_theme| container::Style {
        background: Some(Background::Color(if active {
            ACCENT
        } else {
            Color::TRANSPARENT
        })),
        border: Border {
            radius: border::Radius::new(RADIUS_PILL),
            ..Default::default()
        },
        ..Default::default()
    }
}

pub fn tab(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let hovered = matches!(status, button::Status::Hovered);
        button::Style {
            background: Some(Background::Color(Color::TRANSPARENT)),
            text_color: if active {
                ACCENT
            } else if hovered {
                TEXT
            } else {
                TEXT_DIM
            },
            border: Border::default(),
            ..Default::default()
        }
    }
}

/// The strip that reports what the file says back to the user.
pub fn notice(kind: Notice) -> impl Fn(&Theme) -> container::Style {
    move |_theme| {
        let tint = match kind {
            Notice::Problem => DANGER,
            Notice::Saved => ACCENT,
        };
        container::Style {
            background: Some(Background::Color(alpha(tint, 0.12))),
            text_color: Some(tint),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::Radius::new(RADIUS_CONTROL),
            },
            ..Default::default()
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Notice {
    Problem,
    Saved,
}

// ── controls ─────────────────────────────────────────────────────────────

pub fn quiet_button(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered);
    button::Style {
        background: Some(Background::Color(if hovered { SURFACE_HI } else { INSET })),
        text_color: if hovered { TEXT } else { TEXT_DIM },
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::Radius::new(RADIUS_PILL),
        },
        ..Default::default()
    }
}

pub fn slider_style(_theme: &Theme, status: slider::Status) -> slider::Style {
    let active = !matches!(status, slider::Status::Active);
    slider::Style {
        rail: slider::Rail {
            backgrounds: (
                Background::Color(ACCENT),
                Background::Color(alpha(TEXT_DIM, 0.25)),
            ),
            width: 5.0,
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::Radius::new(RADIUS_PILL),
            },
        },
        handle: slider::Handle {
            shape: slider::HandleShape::Circle { radius: if active { 9.0 } else { 8.0 } },
            background: Background::Color(if active { Color::WHITE } else { ACCENT }),
            border_color: alpha(Color::BLACK, 0.4),
            border_width: 1.0,
        },
    }
}

pub fn toggler_style(_theme: &Theme, status: toggler::Status) -> toggler::Style {
    let on = matches!(
        status,
        toggler::Status::Active { is_toggled: true } | toggler::Status::Hovered { is_toggled: true }
    );
    toggler::Style {
        background: Background::Color(if on { ACCENT } else { alpha(TEXT_DIM, 0.3) }),
        background_border_width: 0.0,
        background_border_color: Color::TRANSPARENT,
        foreground: Background::Color(if on { Color::WHITE } else { alpha(TEXT, 0.85) }),
        foreground_border_width: 0.0,
        foreground_border_color: Color::TRANSPARENT,
        text_color: Some(TEXT),
        // None means perfectly round, which is the shape this look wants.
        border_radius: None,
        padding_ratio: 0.2,
    }
}

pub fn pick_style(_theme: &Theme, status: pick_list::Status) -> pick_list::Style {
    let hovered = matches!(status, pick_list::Status::Hovered | pick_list::Status::Opened { .. });
    pick_list::Style {
        text_color: TEXT,
        placeholder_color: TEXT_DIM,
        handle_color: if hovered { ACCENT } else { TEXT_DIM },
        background: Background::Color(if hovered { SURFACE_HI } else { INSET }),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::Radius::new(RADIUS_CONTROL),
        },
    }
}

pub fn input_style(_theme: &Theme, status: iced::widget::text_input::Status) -> iced::widget::text_input::Style {
    let active = !matches!(status, iced::widget::text_input::Status::Active);
    iced::widget::text_input::Style {
        background: Background::Color(INSET),
        border: Border {
            color: if active { alpha(ACCENT, 0.5) } else { Color::TRANSPARENT },
            width: if active { 1.0 } else { 0.0 },
            radius: border::Radius::new(RADIUS_CONTROL),
        },
        icon: TEXT_DIM,
        placeholder: TEXT_DIM,
        value: TEXT,
        selection: alpha(ACCENT, 0.35),
    }
}

pub fn menu_style(theme: &Theme) -> iced::overlay::menu::Style {
    let base = iced::overlay::menu::default(theme);
    iced::overlay::menu::Style {
        background: Background::Color(SURFACE_HI),
        border: Border {
            color: Color::TRANSPARENT,
            width: 0.0,
            radius: border::Radius::new(RADIUS_CONTROL),
        },
        text_color: TEXT,
        selected_background: Background::Color(alpha(ACCENT, 0.2)),
        selected_text_color: ACCENT,
        ..base
    }
}

pub fn scroller(theme: &Theme, status: scrollable::Status) -> scrollable::Style {
    let base = scrollable::default(theme, status);
    let rail = scrollable::Rail {
        background: None,
        border: Border::default(),
        scroller: scrollable::Scroller {
            background: Background::Color(alpha(TEXT_DIM, 0.35)),
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::Radius::new(RADIUS_PILL),
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
