//! The cusk launcher.
//!
//! A separate client rather than part of the compositor, for the same reason
//! rofi and fuzzel are: text input, fuzzy matching and a scrolling list are an
//! application's problems, and putting them inside the compositor means a bug
//! in any of them takes the whole session down.
//!
//! A **layer-shell panel**, anchored top-right so it sits against the dock and
//! under the compositor's own bar. It is not an xdg-shell window and never
//! reaches cusk's `classify`, so the "exempt from tiling, centred, focused on
//! map" special case that `OVERLAY_APP_ID` still describes is not what puts this
//! on screen — the `LayerMap` is.
//!
//! Styled from `cusk::theme`, so it matches the compositor's chrome and the
//! settings editor without a third copy of the palette.
//!
//! # It needs the compositor to honour keyboard interactivity
//!
//! This asks for `KeyboardInteractivity::Exclusive`, and for a long time cusk
//! honoured no interactivity at all — its `focus()` took a `Window` and read
//! `window.toplevel()`, so a layer surface could never hold the keyboard. Three
//! symptoms, one cause: the search field was inert, Escape never arrived, and
//! because focus was never granted it was never lost, so the panel had no event
//! telling it to disappear and simply stayed on screen. Instances then piled up,
//! one per press of the launcher key.
//!
//! So `Dismiss` below depends on the compositor sending `leave`. Against a
//! compositor that ignores interactivity, this panel is visible and unusable
//! rather than subtly wrong, which is the honest failure of the two.

mod style;

use cusk::config::Config;
use cusk::entry::{self, Entry, Section};
use iced::keyboard::{self, key::Named};
use iced::widget::operation::{self, RelativeOffset};
use iced::widget::{
    button, column, container, image, row, scrollable, space, text, text_input, Id,
};
use iced::{Element, Fill, Length, Subscription, Task};

/// Matches the app id cusk looks for. Changing one without the other turns the
/// launcher back into an ordinary tiled window, which looks like a compositor
/// bug rather than a mismatched string.
const APP_ID: &str = "cusk-launcher";

const INPUT_ID: &str = "query";
/// The application pane's scrollable, so arrowing down can keep the selection
/// on screen.
const LIST_ID: &str = "apps";

/// The HadalOS mark, bundled rather than read from disk.
///
/// `include_bytes!` resolves at compile time, so the source artwork's path —
/// which lives outside this repository — would make the crate build on exactly
/// one machine. `assets/README.md` records that this is a copy and has to be
/// refreshed when the icon is redesigned.
const ICON: &[u8] = include_bytes!("../assets/menu_icon.png");

/// The panel's size, and how wide the category sidebar is inside it.
///
/// Wider and taller than the flat list needed: a sidebar takes width that used
/// to be the application name's, and a two-pane menu with four visible rows is
/// a scrollbar with a heading.
const PANEL: (u32, u32) = (700, 520);
const SIDEBAR: f32 = 160.0;

/// How long the slide takes, and how often it steps.
///
/// Short enough not to be in the way, long enough to read as motion rather
/// than a jump — the point of the animation is to show *where the panel came
/// from*, which is what makes it feel attached to the dock.
const SLIDE_MS: u64 = 160;
const STEP_MS: u64 = 8;

/// Where the top of the panel sits when the compositor's bar is disabled.
///
/// Only used when `appearance.panel-height` is 0. Flush against the screen edge
/// looks like a rendering error, so the panel keeps a hair of margin.
const TOP_WHEN_BARE: i32 = 6;

fn main() -> Result<(), iced_layershell::Error> {
    // Read before the layer surface is created, because the top margin is part
    // of the settings that create it and cannot be corrected later without a
    // visible jump.
    //
    // The bar is drawn *by the compositor*, not by a layer-shell client, so it
    // reserves no exclusive zone and the `LayerMap`'s zone still starts at y=0.
    // That is why this has to be subtracted here at all — and why the old
    // hardcoded `TOP: i32 = 38` was wrong twice over: it did not match the
    // schema's default of 28, and it ignored the setting entirely, so a
    // configured bar of 64 was overlapped and one of 0 left a 38px gap over
    // nothing.
    let config = Config::load(&cusk::config::default_path()).map(|(cfg, _)| cfg).ok();
    let top = match config.as_ref().map(|cfg| cfg.panel_height).unwrap_or(28) {
        0 => TOP_WHEN_BARE,
        height => height.max(0),
    };
    let boot = move || App::boot(config.clone(), top);

    iced_layershell::build_pattern::application(
        boot,
        || APP_ID.to_string(),
        App::update,
        App::view,
    )
    .subscription(App::subscription)
    .style(|_state, _theme| iced::theme::Style {
        // Fully transparent: the panel's own rounded container paints the
        // background, so the corners are actually round rather than square
        // corners over a square window.
        background_color: iced::Color::TRANSPARENT,
        text_color: style::TEXT,
    })
    .settings(iced_layershell::settings::Settings {
        layer_settings: iced_layershell::settings::LayerShellSettings {
            // Pinned to the top-right, beside the dock. Not anchored to the
            // bottom, so the panel keeps its own height instead of being
            // stretched the length of the screen.
            anchor: iced_layershell::reexport::Anchor::Right
                | iced_layershell::reexport::Anchor::Top,
            // Above the dock, which is Top: the launcher is a thing you open
            // *over* the desktop, and it slides out from behind the dock.
            layer: iced_layershell::reexport::Layer::Overlay,
            // Reserves nothing. A launcher that pushed the windows aside
            // every time it opened would rearrange the desktop to show a
            // search box.
            //
            // Reserving nothing is *also* what fixes where it lands, and this
            // is the subtle half: a zone of 0 is `ExclusiveZone::Neutral`, and
            // smithay arranges a Neutral surface inside the **non-exclusive**
            // zone — the screen minus what the dock already reserved. The
            // dock's width has therefore already been taken off before any
            // margin applies, so `attached` below is 0 rather than the dock's
            // 48. Subtracting it a second time is what left the panel floating
            // one dock-width clear of the dock it is meant to hang off.
            exclusive_zone: 0,
            size: Some(PANEL),
            // Starts fully off-screen, one panel-width to the right, and is
            // animated in. `-PANEL.0` rather than `attached` is what makes the
            // first frame a slide instead of a pop.
            margin: (top, -(PANEL.0 as i32), 0, 0),
            // Exclusive: the launcher exists to be typed into, and a search
            // box that does not receive keys is furniture.
            keyboard_interactivity:
                iced_layershell::reexport::KeyboardInteractivity::Exclusive,
            ..Default::default()
        },
        ..Default::default()
    })
    .run()
}

/// One row of the sidebar.
///
/// `All` and `Favourites` are not freedesktop categories and deliberately sit
/// outside `Section`: one is every entry regardless of category, the other is
/// the user's own list. Modelling them as sections would mean inventing two
/// categories no `.desktop` file will ever declare.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Group {
    All,
    Favourites,
    Of(Section),
}

impl Group {
    fn title(self) -> &'static str {
        match self {
            Group::All => "All",
            Group::Favourites => "Favourites",
            Group::Of(section) => section.title(),
        }
    }
}

struct App {
    /// How far off-screen the panel still is, in pixels. Counts down to zero.
    hidden: f32,
    /// The top margin, from `appearance.panel-height`. Held because
    /// `MarginChange` replaces all four margins at once, so every slide step
    /// has to resend it or the panel jumps under the compositor's bar on the
    /// first tick.
    top: i32,
    /// Every entry, kept whole because search spans the entire menu rather than
    /// the selected category — someone who types "term" wants the terminal
    /// wherever it was filed.
    all: Vec<Entry>,
    /// The sidebar and its contents, built once at boot. Entries are cloned into
    /// their groups rather than borrowed: a self-referential struct holding
    /// `&Entry` into its own `all` is not something `view` can be given.
    groups: Vec<(Group, Vec<Entry>)>,
    /// Index into `groups`.
    group: usize,
    query: String,
    /// Index into the *visible* list — the selected group, or the search
    /// results — not into `all`.
    selected: usize,
    /// From the compositor's own config, so a `Terminal=true` entry opens in
    /// the terminal cusk would have spawned rather than a guess.
    terminal: String,
    /// Built once. `Handle::from_bytes` stamps a fresh unique id on every call,
    /// so constructing one per `view` gives the renderer a new texture to
    /// upload every frame and a cache that never stops growing.
    mark: image::Handle,
}

#[iced_layershell::to_layer_message]
#[derive(Debug, Clone)]
enum Message {
    /// One step of the slide.
    Tick,
    Query(String),
    /// Move the selection within the visible list.
    Move(isize),
    /// Move between sidebar categories.
    Cycle(isize),
    /// Click on a sidebar category.
    Pick(usize),
    /// Click on an application row: select it and launch in one step.
    Choose(usize),
    Launch,
    Cancel,
    /// The compositor took the keyboard away, so the launcher is no longer the
    /// thing being used and gets out of the way.
    Dismiss,
}

impl App {
    fn boot(config: Option<Config>, top: i32) -> (Self, Task<Message>) {
        let terminal = config
            .as_ref()
            .map(|cfg| cfg.terminal.clone())
            .unwrap_or_else(|| "auto".into());
        let pinned = config.as_ref().map(|cfg| cfg.dock_pinned.clone()).unwrap_or_default();

        let all = entry::load_all();
        let app = App {
            hidden: PANEL.0 as f32,
            top,
            groups: build_groups(&all, &pinned),
            all,
            group: 0,
            query: String::new(),
            selected: 0,
            terminal: if terminal == "auto" { "foot".into() } else { terminal },
            mark: image::Handle::from_bytes(ICON),
        };
        // Focus the field immediately. A launcher you have to click before
        // typing has failed at the only thing it does.
        (app, operation::focus(Id::new(INPUT_ID)))
    }

    fn subscription(app: &App) -> Subscription<Message> {
        // The timer only runs while there is something to animate. A
        // launcher that ticked forever would keep a core warm to redraw a
        // panel that had already arrived.
        let sliding = if app.hidden > 0.0 {
            iced::time::every(std::time::Duration::from_millis(STEP_MS)).map(|_| Message::Tick)
        } else {
            Subscription::none()
        };
        Subscription::batch([sliding, Self::keys(), Self::focus()])
    }

    fn keys() -> Subscription<Message> {
        // Arrows and Enter are handled here rather than on the text input,
        // because the input consumes neither reliably once a list has focus,
        // and Escape must work whatever is focused.
        //
        // Tab rather than Left/Right for the sidebar. The search field is
        // always focused, and Left/Right are how a cursor moves through what
        // has been typed — binding them to the categories would make the query
        // uneditable, which is a worse trade than a less obvious key.
        keyboard::listen().map(|event| match event {
            keyboard::Event::KeyPressed { key: keyboard::Key::Named(named), modifiers, .. } => {
                match named {
                    Named::ArrowDown => Message::Move(1),
                    Named::ArrowUp => Message::Move(-1),
                    Named::Tab if modifiers.shift() => Message::Cycle(-1),
                    Named::Tab => Message::Cycle(1),
                    Named::Enter => Message::Launch,
                    Named::Escape => Message::Cancel,
                    _ => Message::Move(0),
                }
            }
            // A no-op message rather than filtering: `listen` yields every
            // keyboard event, and mapping the uninteresting ones to a
            // zero-step move costs nothing and keeps the match total.
            _ => Message::Move(0),
        })
    }

    /// Disappear when the compositor hands the keyboard to something else.
    ///
    /// This is the whole of "close when it stops being active". There is no
    /// timer and no polling: `Unfocused` is `wl_keyboard.leave`, which arrives
    /// when a window is clicked, when the desktop is clicked, or when a second
    /// panel takes the keyboard — every way of stopping using the launcher,
    /// reported once, by the only party that knows.
    fn focus() -> Subscription<Message> {
        iced::event::listen_with(|event, _status, _window| match event {
            iced::Event::Window(iced::window::Event::Unfocused) => Some(Message::Dismiss),
            _ => None,
        })
    }

    /// The entries the right-hand pane is showing.
    ///
    /// Search deliberately ignores the selected category and ranks everything:
    /// a query that returned nothing because the wrong sidebar row was
    /// highlighted would look like the application was not installed.
    fn visible(&self) -> Vec<&Entry> {
        if !self.query.trim().is_empty() {
            return entry::rank(&self.all, &self.query);
        }
        self.groups
            .get(self.group)
            .map(|(_, entries)| entries.iter().collect())
            .unwrap_or_default()
    }

    /// Keep the selected row on screen.
    ///
    /// Proportional rather than minimal: snapping to `selected / last` scrolls
    /// slightly more than strictly needed, but it needs no measurement of row
    /// heights or viewport size, which `update` has no access to. Without it,
    /// holding Down walks the selection off the bottom of a long category and
    /// Enter launches something invisible.
    fn follow(&self) -> Task<Message> {
        let count = self.visible().len();
        let y = if count <= 1 {
            0.0
        } else {
            self.selected as f32 / (count - 1) as f32
        };
        operation::snap_to(Id::new(LIST_ID), RelativeOffset { x: None, y: Some(y) })
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                // Linear, and short. Easing would be nicer, but a wrong ease
                // on a 160ms slide reads as a stutter, and this has to be
                // right before it is pretty.
                let per_step = PANEL.0 as f32 * (STEP_MS as f32 / SLIDE_MS as f32);
                self.hidden = (self.hidden - per_step).max(0.0);
                // Only the right margin animates, and it animates to zero —
                // see `exclusive_zone` above for why zero is against the dock
                // rather than against the screen edge. The top margin is
                // already correct and is left alone by `MarginChange`'s
                // caller, which sends all four.
                return Task::done(Message::MarginChange((
                    self.top,
                    -(self.hidden as i32),
                    0,
                    0,
                )));
            }
            Message::Query(query) => {
                self.query = query;
                // Back to the top on every edit. Keeping the index would leave
                // the highlight on whatever now occupies that row, and Enter
                // would launch something the user never looked at.
                self.selected = 0;
                return self.follow();
            }
            Message::Move(delta) => {
                let count = self.visible().len();
                if count > 0 && delta != 0 {
                    let last = count as isize - 1;
                    let next = self.selected as isize + delta;
                    // Clamped, not wrapped. Holding Down should stop at the
                    // bottom rather than silently return to the top, because
                    // the list is long and unlabelled.
                    self.selected = next.clamp(0, last) as usize;
                    return self.follow();
                }
            }
            Message::Cycle(delta) => {
                if self.groups.is_empty() {
                    return Task::none();
                }
                let last = self.groups.len() as isize - 1;
                let next = (self.group as isize + delta).clamp(0, last);
                return self.select_group(next as usize);
            }
            Message::Pick(index) => return self.select_group(index),
            Message::Choose(index) => {
                self.selected = index;
                let chosen = self.visible().get(index).map(|e| (*e).clone());
                if let Some(entry) = chosen {
                    self.launch(&entry);
                    return iced::exit();
                }
            }
            Message::Launch => {
                let chosen = self.visible().get(self.selected).map(|e| (*e).clone());
                if let Some(entry) = chosen {
                    self.launch(&entry);
                    return iced::exit();
                }
            }
            // Both leave by the same door. Kept as separate messages because
            // the causes are different — one is a key, the other is the
            // compositor withdrawing focus — and a single `Close` would make
            // the log unable to say which happened.
            Message::Cancel | Message::Dismiss => return iced::exit(),
            // The protocol actions `to_layer_message` generates. Which
            // variants exist varies with the macro's options, so this is a
            // wildcard rather than a list that breaks on an upgrade. It sits
            // last: placed first, it swallowed every real message above it,
            // and the compiler said so immediately.
            _ => {}
        }
        Task::none()
    }

    /// Switch category, clearing the query.
    ///
    /// Clearing is the point: with a query in the field the right pane shows
    /// search results, so picking a category while one is typed would highlight
    /// a sidebar row and change nothing on screen.
    fn select_group(&mut self, index: usize) -> Task<Message> {
        if index >= self.groups.len() {
            return Task::none();
        }
        self.group = index;
        self.query.clear();
        self.selected = 0;
        self.follow()
    }

    fn launch(&self, entry: &Entry) {
        let mut argv = entry.exec.clone();
        if entry.terminal {
            // `-e` is the one flag foot, alacritty, kitty and konsole agree on.
            let mut wrapped = vec![self.terminal.clone(), "-e".to_string()];
            wrapped.append(&mut argv);
            argv = wrapped;
        }
        let Some((program, args)) = argv.split_first() else { return };

        // Spawned and deliberately not waited for. The launcher exits
        // immediately after, so the child is reparented to init and keeps
        // running — waiting here would keep the launcher window alive for as
        // long as the application it started.
        match std::process::Command::new(program).args(args).spawn() {
            Ok(child) => eprintln!("launched {} (pid {})", entry.name, child.id()),
            Err(e) => eprintln!("could not launch {}: {e}", entry.name),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let visible = self.visible();
        let searching = !self.query.trim().is_empty();

        let mark = image(self.mark.clone())
            .width(Length::Fixed(34.0))
            .height(Length::Fixed(34.0));

        let field = text_input("Search applications", &self.query)
            .id(Id::new(INPUT_ID))
            .on_input(Message::Query)
            .on_submit(Message::Launch)
            .padding([12, 16])
            .size(18)
            .style(style::field);

        container(
            column![
                row![mark, field].spacing(12).align_y(iced::Center),
                row![
                    self.sidebar(searching),
                    container(space())
                        .width(Length::Fixed(1.0))
                        .height(Fill)
                        .style(style::divider),
                    self.pane(&visible),
                ]
                .spacing(10)
                .height(Fill),
                // A count rather than nothing: "no matches" and "the launcher
                // failed to read anything" look identical otherwise.
                text(self.footer(&visible, searching)).size(11).color(style::TEXT_DIM),
            ]
            .spacing(10),
        )
        .padding(14)
        .style(style::window)
        .into()
    }

    /// The category list.
    ///
    /// Dimmed as a whole while a query is typed, because the right pane is then
    /// showing search results from every category and highlighting one of them
    /// would be a lie about what is on screen.
    fn sidebar(&self, searching: bool) -> Element<'_, Message> {
        scrollable(
            column(self.groups.iter().enumerate().map(|(index, (group, entries))| {
                button(
                    row![
                        text(group.title()).size(13),
                        space().width(Fill),
                        // The count is what makes the sidebar worth having: it
                        // says where things are before anything is clicked.
                        text(entries.len().to_string()).size(11).color(style::TEXT_DIM),
                    ]
                    .align_y(iced::Center),
                )
                .padding([7, 10])
                .width(Fill)
                .style(style::category(!searching && index == self.group))
                .on_press(Message::Pick(index))
                .into()
            }))
            .spacing(2),
        )
        .width(Length::Fixed(SIDEBAR))
        .height(Fill)
        .style(style::scroller)
        .into()
    }

    /// The application list for whatever the sidebar or the query selected.
    fn pane(&self, visible: &[&Entry]) -> Element<'_, Message> {
        if visible.is_empty() {
            return container(
                text(if self.all.is_empty() {
                    "No desktop entries found"
                } else {
                    "No matches"
                })
                .size(14)
                .color(style::TEXT_DIM),
            )
            .padding(20)
            .width(Fill)
            .into();
        }

        scrollable(
            column(
                visible
                    .iter()
                    .enumerate()
                    .map(|(index, entry)| self.row_for(entry, index)),
            )
            .spacing(2),
        )
        .id(Id::new(LIST_ID))
        .width(Fill)
        .height(Fill)
        .style(style::scroller)
        .into()
    }

    fn footer(&self, visible: &[&Entry], searching: bool) -> String {
        if searching {
            return format!("{} of {} match", visible.len(), self.all.len());
        }
        match self.groups.get(self.group) {
            Some((group, _)) => {
                format!("{} · {} of {}", group.title(), visible.len(), self.all.len())
            }
            None => "No applications".to_string(),
        }
    }

    fn row_for(&self, entry: &Entry, index: usize) -> Element<'_, Message> {
        let selected = index == self.selected;
        let mut lines = column![text(entry.name.clone()).size(15)].spacing(1);
        if let Some(comment) = &entry.comment {
            lines = lines.push(text(comment.clone()).size(11).color(style::TEXT_DIM));
        }

        button(
            row![
                // The selection marker is a bar, not a highlight box: the
                // references mark a choice with one accent shape, never with a
                // rectangle around it.
                container(space())
                    .width(Length::Fixed(3.0))
                    .height(Length::Fixed(30.0))
                    .style(style::marker(selected)),
                lines,
            ]
            .spacing(12)
            .align_y(iced::Center),
        )
        .padding([7, 10])
        .width(Fill)
        .style(style::menu_row(selected))
        .on_press(Message::Choose(index))
        .into()
    }
}

/// Build the sidebar.
///
/// `All` first because it is what the launcher used to be and what someone
/// reaches for when they do not know the category. `Favourites` next, from the
/// dock's own pinned list rather than a second setting — the applications
/// someone pinned to the dock are, by definition, the ones they use, and asking
/// them to maintain two lists to say so once would be a worse menu.
///
/// Empty groups are dropped by `entry::sections`; `Favourites` is dropped here
/// for the same reason, since a machine with nothing pinned would otherwise
/// show a category that can never contain anything.
fn build_groups(all: &[Entry], pinned: &str) -> Vec<(Group, Vec<Entry>)> {
    // Not sorted here: `entry::load_all` already returns its entries by name,
    // and `entry::sections` sorts each section itself. A third sort would only
    // hide it if either of those stopped.
    let mut groups = vec![(Group::All, all.to_vec())];

    let favourites = entry::resolve_pinned(pinned, all);
    if !favourites.is_empty() {
        groups.push((Group::Favourites, favourites));
    }

    groups.extend(
        entry::sections(all)
            .into_iter()
            .map(|(section, found)| {
                (Group::Of(section), found.into_iter().cloned().collect())
            }),
    );
    groups
}
