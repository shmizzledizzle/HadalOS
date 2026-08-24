//! The cusk shortcut list.
//!
//! `Super + /` — a centred panel showing every binding, grouped, with the
//! modifier rendered as the session actually resolved it.
//!
//! # It renders the table, it does not have one
//!
//! Every row comes from `cusk::bindings::DOCUMENTED`, which is also what the
//! compositor's `resolve` executes and what its startup banner prints. That is
//! the whole design: a cheatsheet with its own copy of the list is a cheatsheet
//! that will eventually lie about the session it is describing, and a shortcut
//! list nobody trusts is worse than none. Before this existed there were three
//! such copies; there is now one table and three renderers.
//!
//! A separate client rather than a mode inside `cusk-launcher`, for the reason
//! the launcher is separate from the compositor: a bug in either takes only
//! itself down, and the launcher's one job stays one job.
//!
//! # Layer shell, and what that requires of the compositor
//!
//! Anchored nowhere — no `Anchor` bits at all, which is how smithay's
//! `LayerMap` centres a surface — on the `Overlay` layer, reserving no
//! exclusive zone, asking for `KeyboardInteractivity::Exclusive`.
//!
//! It dismisses itself on losing keyboard focus, which means it depends on cusk
//! honouring interactivity. That landed in milestone 35; against a compositor
//! that ignores it, this panel is visible and undismissable rather than subtly
//! wrong, which is the honest failure of the two.

mod style;

use cusk::bindings::{self, Group, ModKey};
use cusk::config::Config;
use iced::keyboard::{self, key::Named};
use iced::widget::{column, container, row, scrollable, space, text, Column};
use iced::{Element, Fill, Length, Subscription, Task};

const APP_ID: &str = "cusk-keys";

/// Wide enough for the longest chord and its description side by side, tall
/// enough for the four groups without scrolling on a laptop screen.
///
/// The chord column is sized from the *rendered* text rather than fixed: under
/// `CUSK_MOD=ctrl-alt` every chord grows by nine characters, and a hardcoded
/// column that fit "super + shift + j / k" would clip it.
const PANEL: (u32, u32) = (620, 560);

fn main() -> Result<(), iced_layershell::Error> {
    iced_layershell::build_pattern::application(
        App::boot,
        || APP_ID.to_string(),
        App::update,
        App::view,
    )
    .subscription(App::subscription)
    .style(|_state, _theme| iced::theme::Style {
        // Transparent, so the panel's own rounded container draws the
        // background and the corners are actually round.
        background_color: iced::Color::TRANSPARENT,
        text_color: style::TEXT,
    })
    .settings(iced_layershell::settings::Settings {
        layer_settings: iced_layershell::settings::LayerShellSettings {
            // No anchors. smithay centres a surface anchored to nothing
            // (`desktop/wayland/layer.rs`: with neither LEFT nor RIGHT set, x
            // becomes `(zone.w / 2) - (size.w / 2)`), which is what a reference
            // panel wants — it is not attached to an edge the way the dock and
            // the launcher are.
            anchor: iced_layershell::reexport::Anchor::empty(),
            layer: iced_layershell::reexport::Layer::Overlay,
            // Reserves nothing. Rearranging the desktop to show a reference
            // card would be absurd, and the panel is gone in a second.
            //
            // A zone of 0 is `Neutral`, so the centring happens inside the
            // non-exclusive zone — centred in the space left by the dock rather
            // than on the raw output. That is correct here and worth stating,
            // because it is the same mechanism that put the launcher one
            // dock-width off before milestone 35.
            exclusive_zone: 0,
            size: Some(PANEL),
            keyboard_interactivity:
                iced_layershell::reexport::KeyboardInteractivity::Exclusive,
            ..Default::default()
        },
        ..Default::default()
    })
    .run()
}

struct App {
    /// Pre-rendered rows, grouped. Built once: the table is static and the
    /// modifier cannot change while the panel is open, so rebuilding these per
    /// frame would be formatting the same strings sixty times a second.
    groups: Vec<(Group, Vec<(String, &'static str)>)>,
    /// What the modifier is called, for the footer.
    mod_label: &'static str,
    /// Whether `CUSK_MOD` is in play, so the footer can say so.
    overridden: bool,
}

#[iced_layershell::to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    Cancel,
    /// The compositor took the keyboard away.
    Dismiss,
    /// Every other key. A total match is cheaper than filtering, and this
    /// panel has nothing to do with the rest of them.
    Ignored,
}

impl App {
    fn boot() -> Self {
        // Defaults on a read failure rather than refusing to start. A shortcut
        // list is what someone opens when they do not know what to press, and
        // "your config is unreadable" is the least useful moment to say so —
        // the compositor already reported it at startup. The only thing at risk
        // is the modifier's name.
        let configured = Config::load(&cusk::config::default_path())
            .map(|(cfg, _)| cfg.mod_key)
            .unwrap_or_else(|_| "super".into());

        let mod_key = ModKey::resolve(&configured);
        App {
            groups: bindings::rendered(mod_key.label()),
            mod_label: mod_key.label(),
            // Reported rather than hidden: under a nested session the bindings
            // are not the ones the config file names, and someone comparing
            // this list against `cusk.toml` would otherwise conclude the panel
            // was wrong.
            overridden: std::env::var("CUSK_MOD").is_ok(),
        }
    }

    fn subscription(_app: &App) -> Subscription<Message> {
        Subscription::batch([
            keyboard::listen().map(|event| match event {
                keyboard::Event::KeyPressed { key: keyboard::Key::Named(named), .. } => {
                    match named {
                        // Enter closes as well as Escape. There is nothing here
                        // to confirm, and a reference card that ignores the
                        // most obvious dismissal key feels stuck.
                        Named::Escape | Named::Enter | Named::Space => Message::Cancel,
                        _ => Message::Ignored,
                    }
                }
                _ => Message::Ignored,
            }),
            // The same dismissal the launcher uses: `Unfocused` is
            // `wl_keyboard.leave`, which the compositor sends for every way of
            // stopping using this panel. No timer, no polling.
            iced::event::listen_with(|event, _status, _window| match event {
                iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::Dismiss),
                _ => None,
            }),
        ])
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            // Both leave by the same door. Separate messages because the causes
            // differ — a key, versus the compositor withdrawing focus — and one
            // `Close` would leave the log unable to say which happened.
            Message::Cancel | Message::Dismiss => iced::exit(),
            Message::Ignored => Task::none(),
            // The protocol actions `to_layer_message` generates. Last, not
            // first: placed first it swallows every real message above it.
            _ => Task::none(),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        // Measured across every group, so the description column starts at the
        // same x in all four. Per-group widths would look like four small
        // tables rather than one list.
        let widest = self
            .groups
            .iter()
            .flat_map(|(_, rows)| rows.iter().map(|(chord, _)| chord.chars().count()))
            .max()
            .unwrap_or(0);
        // Monospace digits are not in use, so the column is sized from the
        // character count by an approximate advance. Generous rather than
        // tight: a chord that overflows its column pushes the description out
        // of alignment, and unused space costs nothing.
        let chord_width = (widest as f32 * 7.6) + 12.0;

        let mut body = Column::new().spacing(14);
        for (group, rows) in &self.groups {
            let mut section = column![text(group.title()).size(11).color(style::ACCENT)].spacing(4);
            for (chord, description) in rows {
                section = section.push(
                    row![
                        container(text(chord.clone()).size(13).color(style::TEXT))
                            .width(Length::Fixed(chord_width)),
                        text(*description).size(13).color(style::TEXT_DIM),
                    ]
                    .spacing(10)
                    .align_y(iced::Center),
                );
            }
            body = body.push(section);
        }

        let footer = if self.overridden {
            format!("CUSK_MOD is set — bindings use {} for this session", self.mod_label)
        } else {
            "escape to close".to_string()
        };

        container(
            column![
                row![
                    text("Keyboard shortcuts").size(17).color(style::TEXT),
                    space().width(Fill),
                    text(self.mod_label).size(12).color(style::TEXT_DIM),
                ]
                .align_y(iced::Center),
                scrollable(body).height(Fill).style(style::scroller),
                text(footer).size(11).color(style::TEXT_DIM),
            ]
            .spacing(12),
        )
        .padding(18)
        .style(style::window)
        .into()
    }
}
