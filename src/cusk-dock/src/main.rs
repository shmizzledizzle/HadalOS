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

mod menu;
mod style;
mod tray;

use cusk::entry::{self, Entry};
use iced::widget::{
    button, column, container, image, mouse_area, row, space, svg, text, tooltip,
};
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

/// How wide the surface becomes while a tray menu is open.
///
/// The menu cannot be drawn inside a 48px strip, and it must not be a second
/// layer surface — two surfaces would need their own stacking and their own
/// dismissal, and the menu would be able to outlive the dock that owns it.
///
/// So the dock's surface *widens leftward* while a menu is open. It is anchored
/// `Right`, so growing the width extends it to the left and the strip itself
/// does not move. The **exclusive zone stays at `WIDTH`**, which is what stops
/// windows from being shoved aside every time someone right-clicks a tray icon:
/// the zone is the reservation, and it is deliberately not the surface size.
const MENU_WIDTH: u32 = 260;

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
    /// The tray icon whose menu is open, and the menu itself.
    ///
    /// Held rather than re-fetched per frame: `fetch_menu` is a blocking round
    /// trip, and doing one per redraw would call into another process sixty
    /// times a second to draw a menu that has not changed.
    open_menu: Option<OpenMenu>,
}

/// A tray menu currently on screen.
struct OpenMenu {
    /// Index into `items` at the time it was opened.
    ///
    /// Re-validated before use, not trusted: the tray refreshes every two
    /// seconds and an item can disappear while its menu is open, which would
    /// otherwise send a click to whichever item slid into that position.
    item: usize,
    /// The service the index pointed at, so the re-validation can check it is
    /// still the same item rather than merely still in range.
    service: String,
    entries: Vec<menu::Entry>,
    /// Submenus the user has opened, by row id.
    expanded: Vec<i32>,
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
    /// Right-click: open or close the item's menu.
    OpenMenu(usize),
    /// Middle-click, which is its own action rather than an alias.
    SecondaryActivate(usize),
    /// A row of an open tray menu.
    MenuEntry(i32),
    /// Fold or unfold a submenu.
    ToggleSubmenu(i32),
    /// Dismiss the open menu without choosing anything.
    CloseMenu,
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
            open_menu: None,
        }
    }

    fn namespace() -> String {
        // What cusk logs, and what a user reads when asking which client owns
        // a strip of their screen.
        "cusk-dock".to_string()
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        // Whether a menu was on screen before this message. Compared after
        // handling it, so exactly one place decides when the surface has to
        // change size — every arm that opens or closes a menu would otherwise
        // need to remember to resize, and the one that forgot would leave a
        // 260px invisible surface swallowing clicks over the desktop.
        let was_open = self.open_menu.is_some();
        let task = self.handle(message);
        if self.open_menu.is_some() == was_open {
            return task;
        }
        let width = if self.open_menu.is_some() {
            WIDTH + MENU_WIDTH
        } else {
            WIDTH
        };
        // Height 0 means "as anchored", which is full height here.
        Task::batch([task, Task::done(Message::SizeChange((width, 0)))])
    }

    fn handle(&mut self, message: Message) -> Task<Message> {
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
                    // An open menu whose item has gone is closed rather than
                    // left pointing at a stale index. The tray refreshes on a
                    // timer, so this happens without the user touching
                    // anything — and a menu that outlived its icon would send
                    // its next click to whichever item took that position.
                    if let Some(open) = &self.open_menu {
                        let still_there = self
                            .items
                            .get(open.item)
                            .is_some_and(|i| i.service == open.service);
                        if !still_there {
                            self.open_menu = None;
                        }
                    }
                }
            }
            Message::Activate(index) => {
                if let Some(item) = self.items.get(index) {
                    // An item that says its left click *is* its menu gets one.
                    // These are applications whose icon has no primary action —
                    // a network applet where the menu is the entire point — and
                    // calling `Activate` on them does nothing at all, which
                    // reads as a dead icon.
                    if item.is_menu && item.menu_path.is_some() {
                        return self.update(Message::OpenMenu(index));
                    }
                    tray::activate(item);
                    // Any open menu belongs to the previous interaction.
                    self.open_menu = None;
                }
            }
            Message::OpenMenu(index) => {
                // Clicking the same icon again closes it, which is what every
                // other tray does and what the pointer expects.
                if self.open_menu.as_ref().is_some_and(|open| open.item == index) {
                    self.open_menu = None;
                    return Task::none();
                }
                let Some(item) = self.items.get(index) else { return Task::none() };
                if item.menu_path.is_none() {
                    // No menu is a normal condition, not a failure: plenty of
                    // items offer only a click. Logged at all only because a
                    // right-click that does nothing is otherwise
                    // indistinguishable from one that was not received.
                    eprintln!("tray: {} offers no menu", item.service);
                    return Task::none();
                }
                let entries = tray::fetch_menu(item);
                self.open_menu = (!entries.is_empty()).then(|| OpenMenu {
                    item: index,
                    service: item.service.clone(),
                    entries,
                    expanded: Vec::new(),
                });
            }
            Message::SecondaryActivate(index) => {
                if let Some(item) = self.items.get(index) {
                    tray::secondary_activate(item);
                    self.open_menu = None;
                }
            }
            Message::MenuEntry(id) => {
                // Resolved through the *remembered service* rather than the
                // index alone, so a tray that refreshed between opening the
                // menu and clicking it cannot deliver the click to a different
                // application.
                if let Some(open) = &self.open_menu {
                    let target = self
                        .items
                        .get(open.item)
                        .filter(|item| item.service == open.service);
                    match target {
                        Some(item) => tray::click_menu_entry(item, id),
                        None => eprintln!("tray: the menu's item is gone; ignoring the click"),
                    }
                }
                self.open_menu = None;
            }
            Message::ToggleSubmenu(id) => {
                if let Some(open) = &mut self.open_menu {
                    match open.expanded.iter().position(|held| *held == id) {
                        Some(at) => {
                            open.expanded.remove(at);
                        }
                        None => open.expanded.push(id),
                    }
                }
            }
            Message::CloseMenu => self.open_menu = None,
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

            let open = self.open_menu.as_ref().is_some_and(|m| m.item == index);
            // `mouse_area` rather than `button`, because a button cannot tell
            // the three mouse buttons apart — and right-click is the primary
            // interaction for most tray items. The button stays inside it for
            // the hover and press styling, with its own `on_press` removed so
            // the two do not both fire on a left click.
            let tile = mouse_area(
                button(glyph).padding(4).style(style::tray_tile(open, item.needs_attention())),
            )
            .on_press(Message::Activate(index))
            .on_right_press(Message::OpenMenu(index))
            .on_middle_press(Message::SecondaryActivate(index));

            tooltip(
                tile,
                container(text(item.title.clone()).size(12)).padding(6).style(style::tip),
                tooltip::Position::Left,
            )
            .into()
        }))
        .spacing(4)
        .align_x(iced::Center)
        .into()
    }

    /// The open tray menu, drawn to the left of the strip.
    ///
    /// Rows are flattened by `menu::rows`, so an expanded submenu appears
    /// indented beneath its parent rather than as a flyout. A flyout needs
    /// pointer tracking, a grab, and a decision about which way to open near a
    /// screen edge — none of which this has, and all of which are visibly wrong
    /// when they fail.
    fn menu_panel(&self, open: &OpenMenu) -> Element<'_, Message> {
        let flattened = menu::rows(&open.entries, &open.expanded, 0);

        let rows = flattened.into_iter().map(|(entry, depth)| {
            // Indent by depth, so nesting is legible without a flyout.
            let indent = 8.0 + depth as f32 * 14.0;

            if entry.kind == menu::Kind::Separator {
                return container(space().height(Length::Fixed(1.0)))
                    .padding([4, 8])
                    .width(Fill)
                    .style(style::menu_divider)
                    .into();
            }

            // A check column that is always present, so labels line up whether
            // or not a row has a toggle. Reserving it only for rows that have
            // one makes a mixed menu look ragged.
            let mark = match (entry.toggle, entry.checked) {
                (menu::Toggle::None, _) => " ",
                (_, Some(true)) => "\u{2713}",
                (_, Some(false)) => " ",
                // Indeterminate: the application declined to say, so neither
                // tick nor blank — a dash is the honest third state.
                (_, None) => "\u{2013}",
            };

            let label: Element<Message> = row![
                space().width(Length::Fixed(indent)),
                container(text(mark).size(12)).width(Length::Fixed(14.0)),
                text(entry.label.clone()).size(13),
                space().width(Fill),
                // The submenu affordance. Drawn from `has_submenu` rather than
                // from the child count, so a submenu the application has not
                // populated yet still shows an arrow.
                text(if entry.has_submenu {
                    if open.expanded.contains(&entry.id) { "\u{2304}" } else { "\u{203a}" }
                } else {
                    ""
                })
                .size(12)
                .color(style::TEXT_DIM),
            ]
            .align_y(iced::Center)
            .into();

            // A submenu parent toggles; a leaf sends its click. `clickable`
            // already excludes separators and disabled rows, and a disabled row
            // gets no `on_press` at all rather than one that is ignored — iced
            // draws a button with no handler as disabled, which is exactly the
            // state the application asked for.
            let mut tile = button(label).padding([5, 6]).width(Fill).style(style::menu_row(entry.enabled));
            if entry.has_submenu {
                tile = tile.on_press(Message::ToggleSubmenu(entry.id));
            } else if entry.clickable() {
                tile = tile.on_press(Message::MenuEntry(entry.id));
            }
            tile.into()
        });

        let panel = container(column(rows).spacing(1))
            .padding(6)
            .width(Length::Fixed(MENU_WIDTH as f32 - 10.0))
            .style(style::menu_panel);

        // Bottom-aligned, beside the tray icons the menu belongs to, with the
        // empty space above it closing the menu when clicked. That space is part
        // of this surface while the menu is open, so it would otherwise swallow
        // clicks aimed at the desktop — turning it into the dismissal target
        // makes the only thing it can do the thing a click there should do.
        mouse_area(
            container(column![space().height(Fill), panel].spacing(0))
                .padding([0, 4])
                .height(Fill),
        )
        .on_press(Message::CloseMenu)
        .on_right_press(Message::CloseMenu)
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
        let strip = container(
            column![
                launcher,
                container(apps).height(Fill),
                self.tray_icons(),
            ]
            .spacing(8)
            .align_x(iced::Center),
        )
        .padding(5)
        .width(Length::Fixed(WIDTH as f32))
        .height(Fill)
        .style(style::dock);

        // Only the strip when nothing is open, so the surface is exactly the
        // dock and nothing of the desktop is covered. The menu is added to its
        // left, which is the direction the surface grew.
        match &self.open_menu {
            None => strip.into(),
            Some(open) => row![self.menu_panel(open), strip].height(Fill).into(),
        }
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
