//! The cusk launcher.
//!
//! A separate client rather than part of the compositor, for the same reason
//! rofi and fuzzel are: text input, fuzzy matching and a scrolling list are an
//! application's problems, and putting them inside the compositor means a bug
//! in any of them takes the whole session down.
//!
//! It announces itself as `cusk-launcher`, and cusk special-cases that app id —
//! exempt from tiling, centred, and focused on map. Without that it would
//! arrive as an ordinary window and become a tile, which is the one thing a
//! launcher must never be.
//!
//! Styled from `cusk::theme`, so it matches the compositor's chrome and the
//! settings editor without a third copy of the palette.

mod style;

use cusk::config::Config;
use cusk::entry::{self, Entry};
use iced::keyboard::{self, key::Named};
use iced::widget::operation;
use iced::widget::{column, container, image, row, scrollable, space, text, text_input, Id};
use iced::{Element, Fill, Length, Subscription, Task};

/// Matches the app id cusk looks for. Changing one without the other turns the
/// launcher back into an ordinary tiled window, which looks like a compositor
/// bug rather than a mismatched string.
const APP_ID: &str = "cusk-launcher";

const INPUT_ID: &str = "query";

/// The HadalOS mark, bundled rather than read from disk.
///
/// `include_bytes!` resolves at compile time, so the source artwork's path —
/// which lives outside this repository — would make the crate build on exactly
/// one machine. `assets/README.md` records that this is a copy and has to be
/// refreshed when the icon is redesigned.
const ICON: &[u8] = include_bytes!("../assets/menu_icon.png");

/// The panel's size, and how far it sits from the edges.
///
/// `DOCK` is the dock's width: the launcher stops beside it rather than under
/// it. Duplicated as a constant rather than read from the dock, because a
/// client cannot ask another client how wide it is — the honest fix is for
/// cusk to publish its layout, which is the same IPC the tray will need.
const PANEL: (u32, u32) = (640, 460);
const DOCK: i32 = 48;
const TOP: i32 = 38;

/// How long the slide takes, and how often it steps.
///
/// Short enough not to be in the way, long enough to read as motion rather
/// than a jump — the point of the animation is to show *where the panel came
/// from*, which is what makes it feel attached to the dock.
const SLIDE_MS: u64 = 160;
const STEP_MS: u64 = 8;

fn main() -> Result<(), iced_layershell::Error> {
    iced_layershell::build_pattern::application(
        App::boot,
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
            exclusive_zone: 0,
            size: Some(PANEL),
            // Starts fully off-screen, one panel-width to the right, and is
            // animated in. `-PANEL.0` rather than `0` is what makes the first
            // frame a slide instead of a pop.
            margin: (TOP, -(PANEL.0 as i32), 0, 0),
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

struct App {
    /// How far off-screen the panel still is, in pixels. Counts down to zero.
    hidden: f32,
    all: Vec<Entry>,
    query: String,
    /// Index into the *filtered* list, not into `all`.
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
    Move(isize),
    Launch,
    Cancel,
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let terminal = Config::load(&cusk::config::default_path())
            .map(|(cfg, _)| cfg.terminal)
            .unwrap_or_else(|_| "auto".into());

        let app = App {
            hidden: PANEL.0 as f32,
            all: entry::load_all(),
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
        Subscription::batch([sliding, Self::keys()])
    }

    fn keys() -> Subscription<Message> {
        // Arrows and Enter are handled here rather than on the text input,
        // because the input consumes neither reliably once a list has focus,
        // and Escape must work whatever is focused.
        keyboard::listen().map(|event| match event {
            keyboard::Event::KeyPressed { key: keyboard::Key::Named(named), .. } => match named {
                Named::ArrowDown => Message::Move(1),
                Named::ArrowUp => Message::Move(-1),
                Named::Enter => Message::Launch,
                Named::Escape => Message::Cancel,
                _ => Message::Move(0),
            },
            // A no-op message rather than filtering: `listen` yields every
            // keyboard event, and mapping the uninteresting ones to a
            // zero-step move costs nothing and keeps the match total.
            _ => Message::Move(0),
        })
    }

    fn matches(&self) -> Vec<&Entry> {
        entry::rank(&self.all, &self.query)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Tick => {
                // Linear, and short. Easing would be nicer, but a wrong ease
                // on a 160ms slide reads as a stutter, and this has to be
                // right before it is pretty.
                let per_step = PANEL.0 as f32 * (STEP_MS as f32 / SLIDE_MS as f32);
                self.hidden = (self.hidden - per_step).max(0.0);
                return Task::done(Message::MarginChange((
                    TOP,
                    DOCK - self.hidden as i32,
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
            }
            Message::Move(delta) => {
                let count = self.matches().len();
                if count > 0 {
                    let last = count as isize - 1;
                    let next = self.selected as isize + delta;
                    // Clamped, not wrapped. Holding Down should stop at the
                    // bottom rather than silently return to the top, because
                    // the list is long and unlabelled.
                    self.selected = next.clamp(0, last) as usize;
                }
            }
            Message::Launch => {
                let chosen = self.matches().get(self.selected).map(|e| (*e).clone());
                if let Some(entry) = chosen {
                    self.launch(&entry);
                    return iced::exit();
                }
            }
            Message::Cancel => return iced::exit(),
            // The protocol actions `to_layer_message` generates. Which
            // variants exist varies with the macro's options, so this is a
            // wildcard rather than a list that breaks on an upgrade. It sits
            // last: placed first, it swallowed every real message above it,
            // and the compiler said so immediately.
            _ => {}
        }
        Task::none()
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
        let matches = self.matches();

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

        let list: Element<Message> = if matches.is_empty() {
            container(
                text(if self.all.is_empty() {
                    "No desktop entries found"
                } else {
                    "No matches"
                })
                .size(14)
                .color(style::TEXT_DIM),
            )
            .padding(20)
            .into()
        } else {
            scrollable(
                column(matches.iter().enumerate().map(|(index, entry)| {
                    self.row_for(entry, index == self.selected)
                }))
                .spacing(2),
            )
            .height(Fill)
            .style(style::scroller)
            .into()
        };

        container(
            column![
                row![mark, field].spacing(12).align_y(iced::Center),
                list,
                // A count rather than nothing: "no matches" and "the launcher
                // failed to read anything" look identical otherwise.
                text(format!("{} of {}", matches.len(), self.all.len()))
                    .size(11)
                    .color(style::TEXT_DIM),
            ]
            .spacing(10),
        )
        .padding(14)
        .style(style::window)
        .into()
    }

    fn row_for(&self, entry: &Entry, selected: bool) -> Element<'_, Message> {
        let mut lines = column![text(entry.name.clone()).size(15)].spacing(1);
        if let Some(comment) = &entry.comment {
            lines = lines.push(text(comment.clone()).size(11).color(style::TEXT_DIM));
        }

        container(
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
        .style(style::row(selected))
        .into()
    }
}

