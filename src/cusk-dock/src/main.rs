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
mod session;
// The stage: thumbnails of minimised windows, and the protocol that carries
// them. Declared here as well as in `lib.rs` because this crate builds twice —
// the binary is the dock, the library is what `examples/stageprobe.rs` drives —
// and each build needs its own module tree.
mod stage;
mod stage_protocol;
mod style;
mod tray;
mod windows;

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

/// How wide a thumbnail is drawn in a tooltip.
///
/// Wider than the compositor captures at, on purpose. The capture is bounded
/// at 256 pixels on the long edge and a landscape window comes back close to
/// that wide, so drawing at 200 leaves it very slightly downscaled rather than
/// stretched — and a portrait window, which is captured narrow, is fitted
/// rather than blown up. `ContentFit::Contain` is what enforces the second
/// half of that.
const THUMB: f32 = 200.0;
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

/// Which edge this instance occupies, and therefore what it shows.
///
/// One binary rather than two crates. The two strips share the palette, the
/// desktop-entry reader, the icon resolution and the tile styling; a second
/// crate would duplicate all of it to change an anchor and a view. The
/// *contents* differ — the right strip is launchers and status, the left is
/// what is currently running — and that is a `view` decision, not a program.
///
/// Both instances reserve their own exclusive zone, so windows tile between
/// them rather than under either.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Side {
    /// Pinned launchers, the mark, and the tray. The original dock.
    Right,
    /// Running windows. A taskbar, in the sense milestone 34 meant.
    Left,
}

impl Side {
    /// Parsed from `--side left|right`.
    ///
    /// Defaults to `Right` on anything unrecognised, with a warning, because
    /// that is the strip a session started before this flag existed expects to
    /// get. Failing to start would take out the launcher button and the tray
    /// over a typo in an argument.
    fn from_args() -> Self {
        let mut args = std::env::args().skip(1);
        while let Some(arg) = args.next() {
            let value = match arg.as_str() {
                "--side" => args.next(),
                other => other.strip_prefix("--side=").map(str::to_string),
            };
            let Some(value) = value else { continue };
            return match value.as_str() {
                "left" => Side::Left,
                "right" => Side::Right,
                other => {
                    eprintln!("dock: --side {other:?} is not left or right; using right");
                    Side::Right
                }
            };
        }
        Side::Right
    }

    fn anchor(self) -> Anchor {
        // Top and bottom both, so the strip spans the screen rather than
        // floating in the middle of it. Only the horizontal edge differs.
        match self {
            Side::Right => Anchor::Right | Anchor::Top | Anchor::Bottom,
            Side::Left => Anchor::Left | Anchor::Top | Anchor::Bottom,
        }
    }

    /// What cusk logs, and what a user reads when asking which client owns a
    /// strip of their screen. Distinct per side, or two identical namespaces
    /// make the compositor's log ambiguous about which one mapped.
    fn namespace(self) -> String {
        match self {
            Side::Right => "cusk-dock".to_string(),
            Side::Left => "cusk-dock-windows".to_string(),
        }
    }
}

fn main() -> Result<(), iced_layershell::Error> {
    // Read before the surface is created: the anchor is part of the settings
    // that create it and cannot be changed afterwards without the strip
    // visibly jumping from one edge to the other.
    let side = Side::from_args();

    application(
        move || App::boot(side),
        move || side.namespace(),
        App::update,
        App::view,
    )
        // The tray and window-list threads publish snapshots; this is what
        // notices. A second is slow enough to cost nothing and fast enough that
        // an icon appearing feels immediate.
        .subscription(|_state| {
            iced::time::every(std::time::Duration::from_secs(1)).map(|_| Message::Poll)
        })
        .style(|_state, theme| style::appearance(theme))
        .settings(Settings {
            layer_settings: LayerShellSettings {
                // One horizontal edge, full height. Top and bottom are anchored
                // as well so the strip spans the screen rather than floating in
                // the middle of it.
                anchor: side.anchor(),
                layer: Layer::Top,
                // Matching the width: this is what keeps maximised windows
                // beside the dock instead of underneath it. Both strips reserve
                // their own, so windows tile *between* them.
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
    side: Side,
    pinned: Vec<Pinned>,
    mark: image::Handle,
    launcher: String,
    /// Every installed desktop entry, kept to resolve a window's `app_id` to an
    /// icon. Held rather than re-read: `load_all` walks the filesystem, and the
    /// window list changes far more often than the installed set does.
    installed: Vec<Entry>,
    /// Written by the window-list thread, read here — the same arrangement as
    /// the tray, for the same reason.
    windows: windows::Shared,
    open_windows: Vec<windows::Window>,
    /// Requests queued for the window-list thread. The protocol objects are not
    /// reachable from here, so a click becomes an entry in this.
    outbox: std::sync::Arc<windows::Outbox>,
    /// Written by the window-list thread, read here: a picture of each
    /// minimised window.
    ///
    /// Kept apart from `open_windows` on purpose. The window list is
    /// republished on every title change — which for a terminal is every
    /// command — and a quarter megabyte of pixels riding along on each of
    /// those would be copied thousands of times a session for nothing.
    thumbs: stage::Thumbs,
    /// The same thumbnails as iced image handles, rebuilt only when the pixels
    /// change.
    ///
    /// The cache is the point. `image::Handle::from_rgba` stamps a fresh id on
    /// every call and iced uploads a texture per id it has not seen, so
    /// building these in `view` would re-upload every thumbnail at every
    /// frame — the same mistake `mark` above carries a comment about, and the
    /// one that costs most here because these are the largest images the dock
    /// draws.
    thumb_handles: std::collections::HashMap<u32, (u64, image::Handle)>,
    /// Written by the D-Bus thread, read here. Cloned into the view each tick
    /// rather than held borrowed, because the tray thread must never be
    /// blocked by a slow frame.
    tray: tray::Shared,
    items: Vec<tray::Item>,
    /// What is open in the panel beside the strip, if anything.
    ///
    /// One field rather than a `Option<OpenMenu>` beside a `session_open: bool`.
    /// Both menus draw into the same surface and both widen it by the same
    /// amount, so two independent fields would make "both open" representable —
    /// and it would render one menu over the other while `update` sized the
    /// surface for neither.
    panel: Option<Panel>,
    /// What logind says this session may do, asked once at start-up.
    session: session::Availability,
}

/// The panel beside the strip.
enum Panel {
    /// A tray icon's own menu, fetched over D-Bus from the application.
    Tray(OpenMenu),
    /// The session menu, which is ours and needs no fetching.
    Session,
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
    /// Left-click a window tile: focus it, or unminimise it if it is away.
    FocusWindow(u32),
    /// Right-click a window tile: minimise, or restore if already minimised.
    ToggleMinimize(u32),
    /// Middle-click a window tile: ask it to close.
    CloseWindow(u32),
    /// Open or close the session menu.
    OpenSession,
    /// A row of the session menu.
    SessionAction(session::Action),
}

impl App {
    fn boot(side: Side) -> Self {
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

        // Each strip starts only the protocol it draws. The left one has no
        // tray and the right one no window list, and starting both everywhere
        // would mean two D-Bus watchers fighting for one name and two Wayland
        // connections nothing reads.
        let (windows_shared, outbox, thumbs) = match side {
            Side::Left => windows::start(),
            Side::Right => (
                std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
                windows::Outbox::inert(),
                std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashMap::new())),
            ),
        };
        let tray_shared = match side {
            Side::Right => tray::start(),
            Side::Left => std::sync::Arc::new(std::sync::Mutex::new(Vec::new())),
        };

        App {
            pinned,
            // Built once. `Handle::from_bytes` stamps a fresh id on every call,
            // so building one per view uploads a new texture every frame.
            mark: image::Handle::from_bytes(MARK),
            launcher: cfg.launcher,
            side,
            installed,
            windows: windows_shared,
            open_windows: Vec::new(),
            outbox,
            thumbs,
            // Image handles for what is in `thumbs`, rebuilt only when the
            // pixels change. `Handle::from_rgba` stamps a fresh id per call
            // and iced re-uploads a texture per fresh id, so building these in
            // `view` would upload every thumbnail on every frame.
            thumb_handles: std::collections::HashMap::new(),
            tray: tray_shared,
            items: Vec::new(),
            panel: None,
            // Asked once, here, rather than each time the menu opens: these do
            // not change over a session, and four D-Bus round trips on the
            // click that opens a menu is a menu that hitches.
            session: session::probe(),
        }
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        // Whether a menu was on screen before this message. Compared after
        // handling it, so exactly one place decides when the surface has to
        // change size — every arm that opens or closes a menu would otherwise
        // need to remember to resize, and the one that forgot would leave a
        // 260px invisible surface swallowing clicks over the desktop.
        let was_open = self.panel.is_some();
        let task = self.handle(message);
        if self.panel.is_some() == was_open {
            return task;
        }
        let width = if self.panel.is_some() {
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
                let open = self.windows.lock().map(|w| w.clone()).unwrap_or_default();
                if open != self.open_windows {
                    self.open_windows = open;
                }
                self.refresh_thumbnails();
                let fresh = self.tray.lock().map(|i| i.clone()).unwrap_or_default();
                if fresh != self.items {
                    self.items = fresh;
                    // An open menu whose item has gone is closed rather than
                    // left pointing at a stale index. The tray refreshes on a
                    // timer, so this happens without the user touching
                    // anything — and a menu that outlived its icon would send
                    // its next click to whichever item took that position.
                    if let Some(open) = self.tray_menu() {
                        let still_there = self
                            .items
                            .get(open.item)
                            .is_some_and(|i| i.service == open.service);
                        if !still_there {
                            self.panel = None;
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
                    self.panel = None;
                }
            }
            Message::OpenMenu(index) => {
                // Clicking the same icon again closes it, which is what every
                // other tray does and what the pointer expects.
                if self.tray_menu().is_some_and(|open| open.item == index) {
                    self.panel = None;
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
                self.panel = (!entries.is_empty()).then(|| {
                    Panel::Tray(OpenMenu {
                        item: index,
                        service: item.service.clone(),
                        entries,
                        expanded: Vec::new(),
                    })
                });
            }
            Message::SecondaryActivate(index) => {
                if let Some(item) = self.items.get(index) {
                    tray::secondary_activate(item);
                    self.panel = None;
                }
            }
            Message::MenuEntry(id) => {
                // Resolved through the *remembered service* rather than the
                // index alone, so a tray that refreshed between opening the
                // menu and clicking it cannot deliver the click to a different
                // application.
                if let Some(open) = self.tray_menu() {
                    let target = self
                        .items
                        .get(open.item)
                        .filter(|item| item.service == open.service);
                    match target {
                        Some(item) => tray::click_menu_entry(item, id),
                        None => eprintln!("tray: the menu's item is gone; ignoring the click"),
                    }
                }
                self.panel = None;
            }
            Message::ToggleSubmenu(id) => {
                if let Some(open) = self.tray_menu_mut() {
                    match open.expanded.iter().position(|held| *held == id) {
                        Some(at) => {
                            open.expanded.remove(at);
                        }
                        None => open.expanded.push(id),
                    }
                }
            }
            Message::CloseMenu => self.panel = None,
            Message::OpenSession => {
                // Toggles, like the tray icons: clicking the button that
                // opened a menu is how every other dock closes it.
                self.panel = match self.panel {
                    Some(Panel::Session) => None,
                    _ => Some(Panel::Session),
                };
            }
            Message::SessionAction(action) => {
                // Closed first. `perform` detaches, so the menu would
                // otherwise stay on screen through a suspend and be the
                // first thing visible on resume — over a desktop the user
                // last saw without it.
                self.panel = None;
                session::perform(action);
            }
            Message::FocusWindow(id) => {
                // A minimised window is unminimised rather than activated.
                // Activating one the compositor is not showing would move focus
                // to something invisible — the classic "typing goes nowhere"
                // bug, entered deliberately.
                let minimized = self
                    .open_windows
                    .iter()
                    .find(|w| w.id == id)
                    .is_some_and(|w| w.minimized);
                self.ask(
                    id,
                    if minimized {
                        windows::Request::Unminimize
                    } else {
                        windows::Request::Activate
                    },
                );
            }
            Message::ToggleMinimize(id) => {
                let minimized = self
                    .open_windows
                    .iter()
                    .find(|w| w.id == id)
                    .is_some_and(|w| w.minimized);
                self.ask(
                    id,
                    if minimized {
                        windows::Request::Unminimize
                    } else {
                        windows::Request::Minimize
                    },
                );
            }
            // `close` is a request, not a kill: the application may prompt
            // about unsaved work and may refuse. The tile stays until the
            // compositor reports the window gone, which is the honest
            // indication that nothing has happened yet.
            Message::CloseWindow(id) => self.ask(id, windows::Request::Close),
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

            let open = self.tray_menu().is_some_and(|m| m.item == index);
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

    /// Queue a request for the window-list thread.
    ///
    /// Fire-and-forget. The protocol objects live on that thread; this pushes an
    /// intent and the next turn of its loop sends it. A failed lock is dropped
    /// rather than retried — the alternative is blocking the frame on a mutex
    /// held by a thread that is itself blocked on the compositor.
    fn ask(&self, id: u32, request: windows::Request) {
        // Wakes the event thread as well as queueing, which is the whole point
        // of `Outbox` being a type rather than a `Vec` — see `windows.rs`.
        self.outbox.push(id, request);
    }

    /// Resolve a window's `app_id` to an icon.
    ///
    /// Reuses `resolve_pinned`, which matches a desktop id, then the binary
    /// name, then the visible name — but **`.desktop` has to be appended
    /// first**, and that is not a detail worth discovering twice.
    ///
    /// A desktop entry's id keeps the extension: `org.kde.konsole.desktop`. A
    /// window's `app_id` does not: `org.kde.konsole`. So the id match — the one
    /// that should be exact and reliable — never fired, and every window fell
    /// through to the binary-name attempt. That *happens* to work for
    /// `alacritty` and `konsole`, which is why the pinned list has always looked
    /// fine: those are written as bare binary names. It fails for every
    /// reverse-DNS app id, which is most of them.
    ///
    /// Found with `examples/iconprobe.rs`, because the failure draws a lettered
    /// tile rather than nothing — it reads as "icons do not work here" instead
    /// of as a string with the wrong suffix.
    ///
    /// Built per view rather than cached, and that is a real cost: `svg::Handle`
    /// and `image::Handle::from_path` are cheap because they key on the path,
    /// unlike `from_bytes` which stamps a fresh id per call and grows the
    /// texture cache forever. This is the mistake the launcher icon and the
    /// cursor both made; `from_path` is the version that does not make it.
    fn window_icon(&self, app_id: &str) -> Option<Icon> {
        let app_id = app_id.trim();
        if app_id.is_empty() {
            return None;
        }
        // The suffixed form first, so the exact id match wins over a binary
        // name that happens to collide with a different application.
        let matched = entry::resolve_pinned(&format!("{app_id}.desktop"), &self.installed)
            .into_iter()
            .next()
            .or_else(|| entry::resolve_pinned(app_id, &self.installed).into_iter().next())?;
        let path = matched.icon.as_deref().and_then(entry::find_icon)?;
        Some(if path.extension().is_some_and(|e| e == "svg") {
            Icon::Vector(svg::Handle::from_path(path))
        } else {
            Icon::Raster(image::Handle::from_path(path))
        })
    }

    /// Rebuild the image handles for thumbnails whose pixels changed.
    ///
    /// Called on the poll tick rather than from `view`, because `view` takes
    /// `&self` and because building a handle is an upload rather than a read:
    /// `Handle::from_rgba` stamps a fresh id every call, and iced uploads a
    /// texture for every id it has not seen. Doing this per frame would
    /// re-upload every minimised window sixty times a second.
    ///
    /// The revision is what makes it cheap. Comparing pixels would mean
    /// reading every byte of every thumbnail each tick to establish that
    /// nothing had changed, which is the answer almost every time.
    fn refresh_thumbnails(&mut self) {
        let Ok(held) = self.thumbs.lock() else { return };
        for (id, thumbnail) in held.iter() {
            let cached = self.thumb_handles.get(id).map(|(rev, _)| *rev);
            if cached == Some(thumbnail.revision) {
                continue;
            }
            self.thumb_handles.insert(
                *id,
                (
                    thumbnail.revision,
                    image::Handle::from_rgba(
                        thumbnail.width,
                        thumbnail.height,
                        thumbnail.pixels.clone(),
                    ),
                ),
            );
        }
        // Handles for windows that no longer have a picture go too. Without
        // this the dock holds a texture per window ever minimised for as long
        // as the session lasts — invisible, because nothing draws them.
        self.thumb_handles.retain(|id, _| held.contains_key(id));
    }

    /// The running-window strip.
    ///
    /// One tile per window, in the order they appeared. A taskbar that reorders
    /// itself as focus moves is unusable — the tile you are reaching for moves
    /// as you reach for it — so `windows.rs` publishes first-seen order and this
    /// does not sort.
    fn window_tiles(&self) -> Element<'_, Message> {
        if self.open_windows.is_empty() {
            // Deliberately empty rather than a placeholder. An empty desktop is
            // a normal state, and a strip that said "no windows" would be
            // furniture explaining itself.
            return column![].into();
        }

        column(self.open_windows.iter().map(|window| {
            let glyph: Element<Message> = match self.window_icon(&window.app_id) {
                Some(Icon::Vector(handle)) => svg(handle)
                    .width(Length::Fixed(ICON as f32))
                    .height(Length::Fixed(ICON as f32))
                    .into(),
                Some(Icon::Raster(handle)) => image(handle)
                    .width(Length::Fixed(ICON as f32))
                    .height(Length::Fixed(ICON as f32))
                    .into(),
                None => letter_tile(window.label(), ICON),
            };

            // The focus marker is a bar beside the tile, not a border around it
            // — the same choice the launcher's selected row makes, and for the
            // same reason: one accent shape reads as "this one" where a
            // rectangle reads as a second control.
            let tile = row![
                container(space())
                    .width(Length::Fixed(3.0))
                    .height(Length::Fixed(if window.activated { 26.0 } else { 0.0 }))
                    .style(style::focus_marker(window.activated)),
                mouse_area(
                    button(glyph)
                        .padding(5)
                        .style(style::window_tile(window.activated, window.minimized)),
                )
                .on_press(Message::FocusWindow(window.id))
                .on_right_press(Message::ToggleMinimize(window.id))
                .on_middle_press(Message::CloseWindow(window.id)),
            ]
            .spacing(2)
            .align_y(iced::Center);

            // The picture goes in the tooltip, not on the tile. A tile is 24
            // pixels; a thumbnail at that size is a smudge, and the icon
            // already says which application it is. What a stage is *for* is
            // telling four terminals apart, and that question is only asked
            // while reaching for one — which is exactly when the tooltip is up.
            let label = text(window.label().to_string()).size(12);
            let tip: Element<Message> = match self.thumb_handles.get(&window.id) {
                Some((_, handle)) => column![
                    image(handle.clone())
                        .width(Length::Fixed(THUMB))
                        .content_fit(iced::ContentFit::Contain),
                    label,
                ]
                .spacing(6)
                .align_x(iced::Center)
                .into(),
                None => label.into(),
            };

            tooltip(
                tile,
                container(tip)
                    .padding(6)
                    .style(style::tip),
                // Opens away from the strip, or the tooltip covers the tiles it
                // is describing.
                tooltip::Position::Right,
            )
            .into()
        }))
        .spacing(6)
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
        match self.side {
            Side::Left => self.windows_view(),
            Side::Right => self.launchers_view(),
        }
    }

    /// The left strip: what is running.
    ///
    /// No mark and no tray — those belong to one strip, and duplicating the
    /// launcher button on both would make the desktop look like it had two
    /// docks by accident rather than two by design.
    fn windows_view(&self) -> Element<'_, Message> {
        container(
            column![self.window_tiles(), space().height(Fill)]
                .spacing(8)
                .align_x(iced::Center),
        )
        .padding(5)
        .width(Length::Fixed(WIDTH as f32))
        .height(Fill)
        .style(style::dock)
        .into()
    }

    /// The right strip: the mark, pinned launchers, and the tray.
    fn launchers_view(&self) -> Element<'_, Message> {
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
                self.session_button(),
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
        match &self.panel {
            None => strip.into(),
            Some(Panel::Tray(open)) => row![self.menu_panel(open), strip].height(Fill).into(),
            Some(Panel::Session) => row![self.session_panel(), strip].height(Fill).into(),
        }
    }

    /// The power button, at the foot of the strip.
    ///
    /// Below the tray rather than above it, because it is the one control on
    /// this strip whose misfire is expensive: the bottom corner is the hardest
    /// place to hit by accident on the way to something else.
    ///
    /// The glyph comes from the icon theme, like the tray's do, so it matches
    /// whatever the rest of the desktop is using. `letter_tile` is the fallback
    /// rather than a hardcoded "\u{23FB}" — that codepoint is missing from most
    /// text fonts and renders as a replacement box, which is a worse landmark
    /// than a letter.
    fn session_button(&self) -> Element<'_, Message> {
        let glyph: Element<Message> = ["system-shutdown", "system-log-out", "system-suspend"]
            .into_iter()
            .find_map(entry::find_icon)
            .map(|path| -> Element<Message> {
                if path.extension().is_some_and(|e| e == "svg") {
                    svg(svg::Handle::from_path(path))
                        .width(Length::Fixed(TRAY as f32))
                        .height(Length::Fixed(TRAY as f32))
                        .into()
                } else {
                    image(image::Handle::from_path(path))
                        .width(Length::Fixed(TRAY as f32))
                        .height(Length::Fixed(TRAY as f32))
                        .into()
                }
            })
            .unwrap_or_else(|| letter_tile("Power", TRAY));

        tooltip(
            button(glyph).padding(5).style(style::tile).on_press(Message::OpenSession),
            container(text("Session").size(12)).padding(6).style(style::tip),
            tooltip::Position::Left,
        )
        .into()
    }

    /// The session menu.
    ///
    /// Rendered here rather than through `menu_panel`, which parses
    /// `com.canonical.dbusmenu` layouts an application sent us. These six items
    /// are ours, are known at compile time, and have no ids, submenus or
    /// toggles — routing them through a protocol parser to reuse a row style
    /// would mean inventing a layout to immediately re-read.
    fn session_panel(&self) -> Element<'_, Message> {
        // Lock, Switch User and Suspend leave the session running; the rest end
        // it. Partitioned on `Action::is_final` rather than written out twice,
        // so the divider cannot drift from the meaning it marks — and so adding
        // an action to `Action::ALL` places it correctly without touching this.
        // The split itself is why power menus have a divider at all: Shut Down
        // should not be adjacent to the item most likely to be aimed at.
        let groups: [Vec<session::Action>; 2] = [
            session::Action::ALL.into_iter().filter(|a| !a.is_final()).collect(),
            session::Action::ALL.into_iter().filter(|a| a.is_final()).collect(),
        ];

        let mut rows: Vec<Element<Message>> = Vec::new();
        for (index, group) in groups.iter().enumerate() {
            if index > 0 {
                rows.push(
                    container(space().height(Length::Fixed(1.0)))
                        .padding([4, 8])
                        .width(Fill)
                        .style(style::menu_divider)
                        .into(),
                );
            }
            for &action in group {
                let enabled = self.session.allows(action);
                let label = row![
                    space().width(Length::Fixed(8.0)),
                    text(action.label()).size(13),
                    space().width(Fill),
                ]
                .align_y(iced::Center);

                let mut tile = button(label)
                    .padding([5, 6])
                    .width(Fill)
                    .style(style::menu_row(enabled));
                if enabled {
                    tile = tile.on_press(Message::SessionAction(action));
                }

                // A disabled row with no explanation is indistinguishable from
                // a broken one, and this menu disables things for several
                // different reasons — "no locker installed" and "logind says
                // no" are not the same message.
                rows.push(match self.session.why_not(action) {
                    None => tile.into(),
                    Some(reason) => tooltip(
                        tile,
                        container(text(reason).size(12)).padding(6).style(style::tip),
                        tooltip::Position::Left,
                    )
                    .into(),
                });
            }
        }

        let panel = container(column(rows).spacing(1))
            .padding(6)
            .width(Length::Fixed(MENU_WIDTH as f32 - 10.0))
            .style(style::menu_panel);

        // Bottom-aligned and dismissed by the space above it, exactly as the
        // tray menu is — see `menu_panel` for why that space has to be a
        // dismissal target rather than dead surface.
        mouse_area(
            container(column![space().height(Fill), panel].spacing(0))
                .padding([0, 4])
                .height(Fill),
        )
        .on_press(Message::CloseMenu)
        .on_right_press(Message::CloseMenu)
        .into()
    }

    /// The tray menu, if that is what is open.
    ///
    /// An accessor rather than a field, so `Panel` stays the single place that
    /// knows only one menu can be on screen.
    fn tray_menu(&self) -> Option<&OpenMenu> {
        match &self.panel {
            Some(Panel::Tray(open)) => Some(open),
            _ => None,
        }
    }

    fn tray_menu_mut(&mut self) -> Option<&mut OpenMenu> {
        match &mut self.panel {
            Some(Panel::Tray(open)) => Some(open),
            _ => None,
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
