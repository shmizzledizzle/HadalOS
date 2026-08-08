//! The look, in one file.
//!
//! Modelled on niri's visual language: a near-black background with a faint
//! blue cast, generously rounded surfaces that read as cards rather than
//! panels, a single vivid accent doing all the emphasis, and a lot of space.
//!
//! Everything visual lives here on purpose. The compositor's own chrome —
//! focus rings, borders, corner radii — is meant to adopt the same tokens
//! later, and a palette scattered through view code cannot be adopted by
//! anything. Retuning the aesthetic should be editing this file, not hunting
//! literals through a UI tree.

use iced::border;
use iced::widget::{button, container, pick_list, scrollable, slider, toggler};
use iced::{Background, Border, Color, Shadow, Theme, Vector};

// ── palette ──────────────────────────────────────────────────────────────

/// Window background. Near-black, very slightly blue — flat black reads as a
/// terminal, and a neutral grey reads as a stock toolkit.
pub const BG: Color = rgb(0x13, 0x13, 0x18);
/// Cards and controls sitting on the background.
pub const SURFACE: Color = rgb(0x1C, 0x1C, 0x23);
/// Hover and selected states.
pub const SURFACE_HI: Color = rgb(0x26, 0x26, 0x30);
/// Hairlines. Barely visible by design: the rounding does the separating, and
/// a strong border on a rounded card makes it look like a dialog box.
pub const BORDER: Color = rgb(0x2E, 0x2E, 0x3A);
pub const TEXT: Color = rgb(0xE8, 0xE8, 0xF0);
/// Descriptions and units. Muted enough to recede, light enough to read.
pub const TEXT_DIM: Color = rgb(0x92, 0x92, 0xA6);
/// The one accent. niri's focus ring is the obvious reference point.
pub const ACCENT: Color = rgb(0x7F, 0xC8, 0xFF);
pub const DANGER: Color = rgb(0xFF, 0x6B, 0x6B);
pub const WARNING: Color = rgb(0xFF, 0xC1, 0x4E);

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color {
        r: r as f32 / 255.0,
        g: g as f32 / 255.0,
        b: b as f32 / 255.0,
        a: 1.0,
    }
}

fn alpha(color: Color, a: f32) -> Color {
    Color { a, ..color }
}

// ── metrics ──────────────────────────────────────────────────────────────

/// Card corners. Large enough to be the defining feature rather than a
/// softened edge — this is the single most recognisable thing about the look.
pub const RADIUS_CARD: f32 = 14.0;
pub const RADIUS_CONTROL: f32 = 10.0;
/// Fully round, for pills and slider handles.
pub const RADIUS_PILL: f32 = 999.0;
pub const GAP: f32 = 12.0;
pub const PAD: f32 = 16.0;

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
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: border::Radius::new(RADIUS_CARD),
        },
        // Barely-there lift. A heavy shadow on a dark theme turns into a smear;
        // this only has to separate the card from the background.
        shadow: Shadow {
            color: alpha(Color::BLACK, 0.35),
            offset: Vector::new(0.0, 2.0),
            blur_radius: 12.0,
        },
        ..Default::default()
    }
}

pub fn sidebar(_theme: &Theme) -> container::Style {
    container::Style {
        background: Some(Background::Color(alpha(SURFACE, 0.5))),
        text_color: Some(TEXT),
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: border::Radius::new(RADIUS_CARD),
        },
        ..Default::default()
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
                color: alpha(tint, 0.35),
                width: 1.0,
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

/// Sidebar entries. Selection is a filled pill, which is the niri-ish move —
/// no underline, no chevron, just the shape.
pub fn nav_entry(selected: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |_theme, status| {
        let hovered = matches!(status, button::Status::Hovered);
        button::Style {
            background: Some(Background::Color(if selected {
                alpha(ACCENT, 0.16)
            } else if hovered {
                SURFACE_HI
            } else {
                Color::TRANSPARENT
            })),
            text_color: if selected { ACCENT } else { TEXT },
            border: Border {
                color: Color::TRANSPARENT,
                width: 0.0,
                radius: border::Radius::new(RADIUS_CONTROL),
            },
            ..Default::default()
        }
    }
}

pub fn quiet_button(_theme: &Theme, status: button::Status) -> button::Style {
    let hovered = matches!(status, button::Status::Hovered);
    button::Style {
        background: Some(Background::Color(if hovered { SURFACE_HI } else { SURFACE })),
        text_color: if hovered { TEXT } else { TEXT_DIM },
        border: Border {
            color: BORDER,
            width: 1.0,
            radius: border::Radius::new(RADIUS_CONTROL),
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
        background: Background::Color(if hovered { SURFACE_HI } else { BG }),
        border: Border {
            color: if hovered { alpha(ACCENT, 0.5) } else { BORDER },
            width: 1.0,
            radius: border::Radius::new(RADIUS_CONTROL),
        },
    }
}

pub fn menu_style(theme: &Theme) -> iced::overlay::menu::Style {
    let base = iced::overlay::menu::default(theme);
    iced::overlay::menu::Style {
        background: Background::Color(SURFACE),
        border: Border {
            color: BORDER,
            width: 1.0,
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
