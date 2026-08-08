//! The cusk settings editor.
//!
//! `docs/cusk.md` §4 wants "a GUI and a text file editing the same thing,
//! without fighting". This is the GUI half, and the way it avoids fighting is
//! by not having a model of its own:
//!
//! - **The schema is `cusk::config::SCHEMA`.** Every control on screen is
//!   generated from it — type, range, description and all. There is no list of
//!   widgets to keep in step with the settings, so a setting added to the
//!   compositor appears here with no work at all.
//! - **The file is the state.** Edits go through `set_in_document`, which
//!   mutates one node of a syntax tree and leaves comments, blank lines and
//!   ordering untouched. Nothing is serialised from a struct, so nothing can
//!   be lost by round-tripping.
//! - **There is no apply button and no IPC.** Writing the file is the whole
//!   mechanism: cusk is already watching it, so a change lands in the running
//!   compositor within half a second by the path a hand edit would take.
//!
//! The editor also watches the file, so hand edits show up here live. That is
//! the other half of "without fighting" — a GUI that goes stale the moment you
//! touch the file in an editor is one you stop trusting.

mod style;

use std::path::PathBuf;
use std::time::Duration;

use cusk::config::{self, Complaint, Config, Kind, Setting, Value};
use cusk::toml_edit::DocumentMut;
use iced::widget::{
    button, column, container, pick_list, row, scrollable, slider, space, text, text_input,
    toggler,
};
use iced::{Element, Fill, Length, Subscription, Task};

fn main() -> iced::Result {
    iced::application(App::boot, App::update, App::view)
        .title("cusk settings")
        .theme(theme)
        .subscription(App::subscription)
        .window_size((940.0, 660.0))
        .centered()
        .run()
}

struct App {
    path: PathBuf,
    /// The file, as a syntax tree. Every write goes through this rather than
    /// through a serialiser, which is what keeps the user's comments alive.
    doc: DocumentMut,
    /// The parsed values, and what the controls display. Updated live while a
    /// slider is being dragged, before anything reaches disk.
    config: Config,
    complaints: Vec<Complaint>,
    section: usize,
    notice: Option<(style::Notice, String)>,
    watcher: config::Watcher,
}

#[derive(Debug, Clone)]
enum Message {
    Section(usize),
    /// A value changed in the UI but has not been written yet. Sliders emit
    /// this continuously; writing on every pixel would hammer the disk and
    /// make the compositor reload dozens of times per drag.
    Dragged(&'static str, Value),
    /// Write whatever is currently in `config` for this key.
    Commit(&'static str),
    /// A discrete change — a toggle or a pick — where there is no drag to wait
    /// out, so it is set and written in one step.
    SetNow(&'static str, Value),
    Reset(&'static str),
    Tick,
}

impl App {
    fn boot() -> Self {
        let path = config::default_path();
        if !path.exists() {
            if let Some(dir) = path.parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            let _ = std::fs::write(&path, Config::default_file());
        }
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        let doc = text.parse::<DocumentMut>().unwrap_or_default();
        let (config, complaints) = Config::from_document(&doc);

        App {
            watcher: config::Watcher::new(path.clone()),
            path,
            doc,
            config,
            complaints,
            section: 0,
            notice: None,
        }
    }

    fn subscription(_app: &App) -> Subscription<Message> {
        // Polled for the same reason the compositor polls: an editor saving by
        // rename replaces the inode, and a watch on the old one dies silently.
        iced::time::every(Duration::from_millis(700)).map(|_| Message::Tick)
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Section(index) => self.section = index,
            Message::Dragged(key, value) => {
                // Rejections are impossible here — the controls are built from
                // the same ranges — but silently keeping the old value is the
                // right failure if that ever stops being true.
                let _ = self.config.set(key, value);
            }
            Message::Commit(key) => self.write(key),
            Message::SetNow(key, value) => {
                let _ = self.config.set(key, value);
                self.write(key);
            }
            Message::Reset(key) => {
                if let Some(setting) = Config::setting(key) {
                    let _ = self.config.set(key, setting.kind.default_value());
                    self.write(key);
                }
            }
            Message::Tick => self.absorb_external_edits(),
        }
        Task::none()
    }

    /// Write one setting to disk.
    fn write(&mut self, key: &str) {
        let Some(value) = self.config.get(key) else { return };
        if let Err(problem) = config::set_in_document(&mut self.doc, key, value) {
            self.notice = Some((style::Notice::Problem, format!("{key}: {problem}")));
            return;
        }
        match std::fs::write(&self.path, self.doc.to_string()) {
            Ok(()) => {
                // Take the file as it now stands *without* treating it as an
                // external edit. Otherwise the next tick reads back our own
                // save and rebuilds state from disk on top of whatever the
                // user is in the middle of doing.
                self.watcher.resync();
                let (_, complaints) = Config::from_document(&self.doc);
                self.complaints = complaints;
                self.notice = Some((style::Notice::Saved, format!("Saved {key}")));
            }
            Err(e) => {
                self.notice = Some((style::Notice::Problem, format!("Could not write: {e}")));
            }
        }
    }

    /// Pick up edits made in a text editor while this window is open.
    fn absorb_external_edits(&mut self) {
        match self.watcher.check_now() {
            config::Reload::Unchanged => {}
            config::Reload::Applied { config, complaints } => {
                let text = std::fs::read_to_string(&self.path).unwrap_or_default();
                if let Ok(doc) = text.parse::<DocumentMut>() {
                    self.doc = doc;
                }
                self.config = config;
                self.complaints = complaints;
                self.notice = Some((style::Notice::Saved, "Reloaded from disk".into()));
            }
            // A partial save is a normal intermediate state, not a reason to
            // throw away what is on screen.
            config::Reload::Failed(e) => {
                self.notice = Some((style::Notice::Problem, e.lines().next().unwrap_or("").into()));
            }
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let sections = config::sections();

        // A top tab row with an accent underline, not a sidebar. Both
        // reference shells mark the active section with one thin line and
        // nothing else — no box, no chevron, no filled panel.
        let tabs = row(sections.iter().enumerate().map(|(index, name)| {
            let active = index == self.section;
            column![
                button(text(heading(name)).size(14))
                    .width(Fill)
                    .padding([8, 4])
                    .style(style::tab(active))
                    .on_press(Message::Section(index)),
                container(space())
                    .height(Length::Fixed(2.0))
                    .width(Fill)
                    .style(style::tab_underline(active)),
            ]
            .spacing(6)
            .width(Length::Fixed(118.0))
            .into()
        }))
        .spacing(4);

        let header = row![
            column![
                text("cusk").size(21),
                text("settings").size(12).color(style::TEXT_DIM),
            ]
            .spacing(1),
            space().width(Fill),
            tabs,
        ]
        .align_y(iced::Bottom)
        .spacing(style::GAP);

        let current = sections.get(self.section).copied().unwrap_or("layout");
        let cards = column(
            config::settings_in(current)
                .into_iter()
                .map(|setting| self.card(setting)),
        )
        .spacing(style::GAP);

        let body = scrollable(container(cards).padding([0, 6]))
            .height(Fill)
            .style(style::scroller);

        container(
            column![header, body, self.footer()]
                .spacing(style::GAP)
                .height(Fill),
        )
        .padding(style::PAD)
        .style(style::window)
        .into()
    }

    /// One setting, rendered from its schema entry alone.
    fn card(&self, setting: &'static Setting) -> Element<'_, Message> {
        let key = setting.key;
        let value = self.config.get(key).unwrap_or_else(|| setting.kind.default_value());
        let is_default = value == setting.kind.default_value();

        let mut heading_row = row![
            text(config::label_of(setting)).size(16),
            space().width(Fill),
        ]
        .spacing(8)
        .align_y(iced::Center);

        // Only offered when it would do something. A permanently visible
        // "Reset" next to a value already at its default is noise that teaches
        // people to ignore the control.
        if !is_default {
            heading_row = heading_row.push(
                button(text("Reset").size(12))
                    .padding([4, 10])
                    .style(style::quiet_button)
                    .on_press(Message::Reset(key)),
            );
        }

        let mut lines = column![
            heading_row,
            text(setting.doc).size(13).color(style::TEXT_DIM),
        ]
        .spacing(6);

        if setting.apply == config::Apply::Restart {
            // Said on the control itself, not buried in documentation. A
            // setting that visibly changes and does nothing is the worst
            // outcome the GUI can produce.
            lines = lines.push(
                text("Takes effect on restart")
                    .size(12)
                    .color(style::WARNING),
            );
        }

        lines = lines.push(self.control(setting, &value));

        container(lines)
            .padding(style::PAD)
            .width(Fill)
            .style(style::card)
            .into()
    }

    /// The control for a setting, chosen by its declared type.
    fn control(&self, setting: &'static Setting, value: &Value) -> Element<'_, Message> {
        let key = setting.key;
        match setting.kind {
            Kind::Int { min, max, .. } => {
                let current = match value {
                    Value::Int(v) => *v,
                    _ => min,
                };
                row![
                    slider(min..=max, current, move |v| Message::Dragged(key, Value::Int(v)))
                        .on_release(Message::Commit(key))
                        .style(style::slider_style),
                    text(format!("{current}"))
                        .size(14)
                        .width(Length::Fixed(52.0))
                        .align_x(iced::Right),
                ]
                .spacing(style::GAP)
                .align_y(iced::Center)
                .into()
            }
            Kind::Float { min, max, .. } => {
                let current = match value {
                    Value::Float(v) => *v,
                    _ => min,
                };
                row![
                    slider(min..=max, current, move |v| Message::Dragged(key, Value::Float(v)))
                        .step(0.01)
                        .on_release(Message::Commit(key))
                        .style(style::slider_style),
                    text(format!("{current:.2}"))
                        .size(14)
                        .width(Length::Fixed(52.0))
                        .align_x(iced::Right),
                ]
                .spacing(style::GAP)
                .align_y(iced::Center)
                .into()
            }
            Kind::Bool { .. } => {
                let current = matches!(value, Value::Bool(true));
                toggler(current)
                    .on_toggle(move |v| Message::SetNow(key, Value::Bool(v)))
                    .style(style::toggler_style)
                    .into()
            }
            Kind::Text { .. } => {
                let current = match value {
                    Value::Text(v) => v.clone(),
                    _ => String::new(),
                };
                column![
                    text_input("", &current)
                        .on_input(move |v| Message::Dragged(key, Value::Text(v)))
                        // Committed on Enter, not per keystroke. Writing the
                        // file on every character would have the compositor
                        // trying to load "/home/u", "/home/us", "/home/use"
                        // and warning about each one.
                        .on_submit(Message::Commit(key))
                        .padding([8, 12])
                        .style(style::input_style),
                    text("Press Enter to apply").size(11).color(style::TEXT_DIM),
                ]
                .spacing(5)
                .into()
            }
            Kind::Choice { options, .. } => {
                let choices: Vec<String> = options.iter().map(|o| o.to_string()).collect();
                let current = match value {
                    Value::Text(v) => Some(v.clone()),
                    _ => None,
                };
                pick_list(choices, current, move |v: String| {
                    Message::SetNow(key, Value::Text(v))
                })
                .padding([8, 12])
                .style(style::pick_style)
                .menu_style(style::menu_style)
                .into()
            }
        }
    }

    fn footer(&self) -> Element<'_, Message> {
        // Complaints outrank the transient "Saved" note: a key the file got
        // wrong stays wrong until someone fixes it, whereas the note is about
        // something that already succeeded.
        if let Some(complaint) = self.complaints.first() {
            let extra = self.complaints.len().saturating_sub(1);
            let message = if extra > 0 {
                format!("{complaint}  (and {extra} more)")
            } else {
                complaint.to_string()
            };
            return container(text(message).size(13))
                .padding([8, 12])
                .width(Fill)
                .style(style::notice(style::Notice::Problem))
                .into();
        }

        match &self.notice {
            Some((kind, message)) => container(text(message.clone()).size(13))
                .padding([8, 12])
                .width(Fill)
                .style(style::notice(*kind))
                .into(),
            None => container(
                text(self.path.display().to_string())
                    .size(12)
                    .color(style::TEXT_DIM),
            )
            .padding([8, 12])
            .width(Fill)
            .into(),
        }
    }
}

/// A named function rather than a closure: `theme` is required to be generic
/// over the state's lifetime, and an inline closure gets inferred at one
/// specific lifetime instead.
fn theme(_app: &App) -> iced::Theme {
    style::theme()
}

fn heading(section: &str) -> String {
    let mut out = section.to_string();
    if let Some(first) = out.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    out
}
