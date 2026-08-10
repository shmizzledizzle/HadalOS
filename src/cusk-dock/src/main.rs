//! The cusk dock.
//!
//! A vertical strip of applications down the right-hand edge, with the
//! launcher attached at the top — the arrangement in the KaOS/Niri reference
//! screenshots.
//!
//! It is a **layer-shell client**, not something the compositor draws. cusk's
//! own top panel is drawn in-process only because iced cannot speak
//! `wlr-layer-shell`; `iced_layershell` is what removes that constraint, so the
//! dock is an ordinary program that can be replaced, restarted or rewritten
//! without touching the compositor.
//!
//! Anchored right and reserving an exclusive zone, so windows tile and maximise
//! beside it rather than under it. cusk honours that zone through
//! `LayerMap::non_exclusive_zone`, which is the same path waybar exercises.

mod style;
mod tray;

use cusk::entry::{self, Entry};
use iced::widget::{button, column, container, image, svg, text, tooltip};
use iced::{Element, Fill, Length, Task};
use iced_layershell::build_pattern::application;
use iced_layershell::reexport::{Anchor, KeyboardInteractivity, Layer};
use iced_layershell::settings::{LayerShellSettings, Settings};
use iced_layershell::to_layer_message;

/// Width of the strip, and the exclusive zone it reserves. One constant, so
/// the space claimed and the space drawn cannot disagree.
///
/// Narrow on purpose: the KaOS reference is a bar of icons, not a shelf. The
/// application list belongs in the launcher that opens beside it.
const WIDTH: u32 = 48;
const ICON: u16 = 26;
/// The mark is a little larger than the pins: it is the one fixed landmark on
/// the strip, and everything else is found relative to it.
const MARK_SIZE: u16 = 30;
/// Tray icons are smaller than pins: they are status, not destinations.
const TRAY: u16 = 20;

/// The HadalOS mark, for the launcher button.
const MARK: &[u8] = include_bytes!("../../cusk-launcher/assets/menu_icon.png");

fn main() -> Result<(), iced_layershell::Error> {
    application(App::boot, App::namespace, App::update, App::view)
        // The tray thread publishes a snapshot; this is what notices. A
        // second is slow enough to cost nothing and fast enough that an icon
        // appearing feels immediate.
        .subscription(|_state| {
            iced::time::every(std::time::Duration::from_secs(1)).map(|_| Message::Poll)
        })
        .style(|_state, theme| style::appearance(theme))
        .settings(Settings {
            layer_settings: LayerShellSettings {
                // Right edge, full height. Top and bottom are anchored as well
                // so the strip spans the screen rather than floating in the
                // middle of it.
                anchor: Anchor::Right | Anchor::Top | Anchor::Bottom,
                layer: Layer::Top,
                // Matching the width: this is what keeps maximised windows
                // beside the dock instead of underneath it.
                exclusive_zone: WIDTH as i32,
                size: Some((WIDTH, 0)),
                // The dock is clicked, never typed into. Taking keyboard focus
                // would steal it from the terminal the user is working in
                // every time they reached for an icon.
                keyboard_interactivity: KeyboardInteractivity::None,
                ..Default::default()
            },
            ..Default::default()
        })
        .run()
}

/// A pinned application, with its icon already located.
struct Pinned {
    entry: Entry,
    icon: Option<Icon>,
}

enum Icon {
    Raster(image::Handle),
    Vector(svg::Handle),
}

struct App {
    pinned: Vec<Pinned>,
    mark: image::Handle,
    launcher: String,
    /// Written by the D-Bus thread, read here. Cloned into the view each tick
    /// rather than held borrowed, because the tray thread must never be
    /// blocked by a slow frame.
    tray: tray::Shared,
    items: Vec<tray::Item>,
}

#[to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    Launch(usize),
    OpenLauncher,
    /// Re-read the tray snapshot.
    Poll,
    /// Left-click on a tray icon.
    Activate(usize),
}

impl App {
    fn boot() -> Self {
        let cfg = cusk::config::Config::load(&cusk::config::default_path())
            .map(|(cfg, _)| cfg)
            .unwrap_or_default();

        // Pinned only. The first version listed everything installed, which
        // made the dock a second launcher sorted alphabetically — it answered
        // "what is on this machine" when a dock's question is "what do I use".
        let installed = entry::load_all();
        let pinned = entry::resolve_pinned(&cfg.dock_pinned, &installed)
            .into_iter()
            .map(|entry| {
                let icon = entry
                    .icon
                    .as_deref()
                    .and_then(entry::find_icon)
                    .map(|path| {
                        if path.extension().is_some_and(|e| e == "svg") {
                            Icon::Vector(svg::Handle::from_path(path))
                        } else {
                            Icon::Raster(image::Handle::from_path(path))
                        }
                    });
                Pinned { entry, icon }
            })
            .collect();

        App {
            pinned,
            // Built once. `Handle::from_bytes` stamps a fresh id on every call,
            // so building one per view uploads a new texture every frame.
            mark: image::Handle::from_bytes(MARK),
            launcher: cfg.launcher,
            tray: tray::start(),
            items: Vec::new(),
        }
    }

    fn namespace() -> String {
        // What cusk logs, and what a user reads when asking which client owns
        // a strip of their screen.
        "cusk-dock".to_string()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Launch(index) => {
                if let Some(pinned) = self.pinned.get(index) {
                    spawn(&pinned.entry.exec);
                }
            }
            Message::OpenLauncher => spawn(std::slice::from_ref(&self.launcher)),
            Message::Poll => {
                // Compared before assigning: iced rebuilds the view whenever
                // state changes, and copying an identical list every second
                // would redraw the dock forever for nothing.
                let fresh = self.tray.lock().map(|i| i.clone()).unwrap_or_default();
                if fresh != self.items {
                    self.items = fresh;
                }
            }
            Message::Activate(index) => {
                if let Some(item) = self.items.get(index) {
                    tray::activate(item);
                }
            }
            // Generated by `to_layer_message` for the protocol's own actions;
            // the dock issues none of them.
            _ => {}
        }
        Task::none()
    }

    /// The tray, along the bottom.
    ///
    /// A theme name is resolved through the same lookup desktop entries use,
    /// so a tray icon matches the rest of the desktop instead of being
    /// whatever the application happened to ship. Only when there is no name
    /// are the raw pixels used.
    fn tray_icons(&self) -> Element<'_, Message> {
        column(self.items.iter().enumerate().map(|(index, item)| {
            let glyph: Element<Message> = match (&item.icon_name, &item.pixmap) {
                (Some(name), _) => match entry::find_icon(name) {
                    Some(path) if path.extension().is_some_and(|e| e == "svg") => {
                        svg(svg::Handle::from_path(path))
                            .width(Length::Fixed(TRAY as f32))
                            .height(Length::Fixed(TRAY as f32))
                            .into()
                    }
                    Some(path) => image(image::Handle::from_path(path))
                        .width(Length::Fixed(TRAY as f32))
                        .height(Length::Fixed(TRAY as f32))
                        .into(),
                    // Named an icon this desktop does not have. The initial is
                    // still better than a blank square, and it is clickable.
                    None => letter_tile(&item.title, TRAY),
                },
                (None, Some(pixmap)) => image(image::Handle::from_rgba(
                    pixmap.width,
                    pixmap.height,
                    pixmap.rgba.clone(),
                ))
                .width(Length::Fixed(TRAY as f32))
                .height(Length::Fixed(TRAY as f32))
                .into(),
                (None, None) => letter_tile(&item.title, TRAY),
            };

            tooltip(
                button(glyph).padding(4).style(style::tile).on_press(Message::Activate(index)),
                container(text(item.title.clone()).size(12)).padding(6).style(style::tip),
                tooltip::Position::Left,
            )
            .into()
        }))
        .spacing(4)
        .align_x(iced::Center)
        .into()
    }

    fn view(&self) -> Element<'_, Message> {
        let launcher = tooltip(
            // Both dimensions, like the pinned icons below. Setting only the
            // width leaves the height at iced's default of `Fill`, which
            // collapses to nothing inside a shrink-height column — the mark
            // was measured, laid out, and drawn zero pixels tall.
            button(
                image(self.mark.clone())
                    .width(Length::Fixed(MARK_SIZE as f32))
                    .height(Length::Fixed(MARK_SIZE as f32)),
            )
            .padding(6)
            .style(style::tile)
            .on_press(Message::OpenLauncher),
            container(text("Applications").size(12))
                .padding(6)
                .style(style::tip),
            tooltip::Position::Left,
        );

        let apps = column(self.pinned.iter().enumerate().map(|(index, pinned)| {
            let glyph: Element<Message> = match &pinned.icon {
                Some(Icon::Vector(handle)) => svg(handle.clone())
                    .width(Length::Fixed(ICON as f32))
                    .height(Length::Fixed(ICON as f32))
                    .into(),
                Some(Icon::Raster(handle)) => image(handle.clone())
                    .width(Length::Fixed(ICON as f32))
                    .height(Length::Fixed(ICON as f32))
                    .into(),
                // A lettered tile rather than a gap, so an icon that could not
                // be resolved is visibly unresolved instead of invisible.
                None => container(
                    text(pinned.entry.name.chars().next().unwrap_or('?').to_string()).size(16),
                )
                .center_x(Length::Fixed(ICON as f32))
                .center_y(Length::Fixed(ICON as f32))
                .style(style::letter)
                .into(),
            };

            tooltip(
                button(glyph).padding(5).style(style::tile).on_press(Message::Launch(index)),
                container(text(pinned.entry.name.clone()).size(12))
                    .padding(6)
                    .style(style::tip),
                tooltip::Position::Left,
            )
            .into()
        }))
        .spacing(6)
        .align_x(iced::Center);

        // Mark at the top, pins beneath it, and the tray region held open at
        // the bottom — the KaOS arrangement. `Fill` on the middle is what
        // pushes the bottom group down without a hardcoded height.
        container(
            column![
                launcher,
                container(apps).height(Fill),
                self.tray_icons(),
            ]
            .spacing(8)
            .align_x(iced::Center),
        )
        .padding(5)
        .height(Fill)
        .style(style::dock)
        .into()
    }
}


/// A lettered square, for anything whose icon could not be found.
///
/// Used rather than a blank: an invisible button is indistinguishable from a
/// missing feature, and this one is still clickable.
fn letter_tile<'a>(name: &str, size: u16) -> Element<'a, Message> {
    container(text(name.chars().next().unwrap_or('?').to_string()).size(12))
        .center_x(Length::Fixed(size as f32))
        .center_y(Length::Fixed(size as f32))
        .style(style::letter)
        .into()
}

/// Start a program, detached.
///
/// Reaped on a thread: a dock that spawns and never waits accumulates a zombie
/// per launch, and over a session that fills the process table — which looks
/// like anything except a dock bug.
fn spawn(argv: &[String]) {
    let Some((program, args)) = argv.split_first() else { return };
    match std::process::Command::new(program).args(args).spawn() {
        Ok(child) => {
            std::thread::spawn(move || {
                let mut child = child;
                let _ = child.wait();
            });
        }
        Err(e) => eprintln!("could not launch {program}: {e}"),
    }
}
