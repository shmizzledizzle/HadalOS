//! Telling other clients what windows exist.
//!
//! This is what turns cusk-dock from a launcher into a taskbar. The dock today
//! knows only the `.desktop` files it was configured with and whatever claims a
//! tray icon; it has no idea which windows are open, so it cannot list them,
//! indicate them, or focus them. That is a missing protocol, not a missing
//! stylesheet.
//!
//! # Which protocol, and why not the newer one
//!
//! There are two, and the tempting choice is wrong:
//!
//! - **`ext-foreign-toplevel-list-v1`** is the standardised one, and smithay
//!   0.7 already implements it — `wayland::foreign_toplevel_list` is right
//!   there, which makes it look like the cheap answer. It is **read-only**. It
//!   enumerates windows and reports title and app-id, and offers no way to
//!   focus, close or minimise one. A taskbar whose entries cannot be clicked is
//!   a status display.
//! - **`zwlr_foreign_toplevel_management_v1`** carries the requests that matter
//!   — `activate`, `close`, `set_minimized`, `set_maximized`, `set_fullscreen`.
//!   smithay does not implement it, so the server side is ours to write against
//!   `wayland-protocols-wlr`, which is already in the dependency graph via
//!   layer-shell.
//!
//! So: more work, and the only option that produces the thing being asked for.
//! Every desktop whose taskbar cusk is being measured against — Plasma, Hyprland,
//! niri, sway — speaks the wlr one for exactly this reason.
//!
//! It is worth writing down that this is a *privileged* protocol in spirit if
//! not in the specification: any client that binds the global can enumerate and
//! control every window. The dock is a client like any other, and nothing here
//! restricts who may bind. That is how every other compositor ships it and it
//! is still worth stating rather than discovering.
//!
//! # Why this module is pure
//!
//! The protocol is not "send the current state". It is "send what changed, then
//! `done`", and getting that wrong is invisible in the good case: a client that
//! receives a redundant `title` event looks fine, and a client that never
//! receives a `done` silently never updates. So the diff is separated from the
//! plumbing and tested without a display, the same way `panel.rs` keeps its
//! geometry testable.

use std::collections::BTreeSet;

/// The four states the protocol can report. Values are the protocol's own
/// enum, because they go on the wire as a `uint` array.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u32)]
pub enum State {
    Maximized = 0,
    Minimized = 1,
    Activated = 2,
    Fullscreen = 3,
}

/// Outputs are identified by index rather than by a Wayland object, so this
/// module stays free of protocol types and testable.
pub type OutputId = usize;

/// Everything the protocol can say about one window, at one instant.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub title: String,
    pub app_id: String,
    pub states: BTreeSet<State>,
    /// Outputs this window is visible on. A `BTreeSet` so that enter/leave
    /// diffs are order-independent — a window that moved between two outputs
    /// must not emit events merely because the compositor iterated differently.
    pub outputs: BTreeSet<OutputId>,
}

/// One protocol event, in the order it must be sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Title(String),
    AppId(String),
    OutputEnter(OutputId),
    OutputLeave(OutputId),
    /// The whole state array, not a delta — the protocol replaces it wholesale.
    State(Vec<State>),
    /// Terminates every burst. Clients are entitled to treat everything before
    /// it as one atomic update.
    Done,
}

/// What to send to bring a client from `prev` to `next`.
///
/// `prev == None` means the handle has just been advertised and the client
/// knows nothing, so everything is sent.
///
/// Returns **empty** when nothing changed. Not `[Done]` — a bare `done` is a
/// wakeup with no information, and on a busy desktop the compositor calls this
/// on every commit. An idle window should cost an idle dock nothing.
pub fn diff(prev: Option<&Snapshot>, next: &Snapshot) -> Vec<Event> {
    let mut out = Vec::new();

    match prev {
        None => {
            // Order matters less than completeness here, but title first
            // matches what every other compositor sends and keeps captures
            // comparable when debugging against a known-good one.
            out.push(Event::Title(next.title.clone()));
            out.push(Event::AppId(next.app_id.clone()));
            for &output in &next.outputs {
                out.push(Event::OutputEnter(output));
            }
            out.push(Event::State(next.states.iter().copied().collect()));
        }
        Some(prev) => {
            if prev.title != next.title {
                out.push(Event::Title(next.title.clone()));
            }
            if prev.app_id != next.app_id {
                out.push(Event::AppId(next.app_id.clone()));
            }
            // Leave before enter. A window moving from output 0 to output 1
            // that announced the enter first would, for one event, claim to be
            // on both — and a dock grouping by output would double-count it.
            for &output in prev.outputs.difference(&next.outputs) {
                out.push(Event::OutputLeave(output));
            }
            for &output in next.outputs.difference(&prev.outputs) {
                out.push(Event::OutputEnter(output));
            }
            if prev.states != next.states {
                out.push(Event::State(next.states.iter().copied().collect()));
            }
        }
    }

    if !out.is_empty() {
        out.push(Event::Done);
    }
    out
}

impl Snapshot {
    pub fn is_activated(&self) -> bool {
        self.states.contains(&State::Activated)
    }

    pub fn is_minimized(&self) -> bool {
        self.states.contains(&State::Minimized)
    }

    /// What a taskbar shows when the title is empty.
    ///
    /// An untitled window is common — it is what a client looks like between
    /// mapping and its first `set_title` — and a dock entry with no label is
    /// indistinguishable from a rendering bug. The app-id is the better
    /// fallback because it is what the user would call the program.
    pub fn label(&self) -> &str {
        if !self.title.is_empty() {
            &self.title
        } else if !self.app_id.is_empty() {
            &self.app_id
        } else {
            "(untitled)"
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snap(title: &str, app_id: &str, states: &[State], outputs: &[OutputId]) -> Snapshot {
        Snapshot {
            title: title.into(),
            app_id: app_id.into(),
            states: states.iter().copied().collect(),
            outputs: outputs.iter().copied().collect(),
        }
    }

    #[test]
    fn a_new_handle_is_told_everything() {
        let s = snap("Konsole", "org.kde.konsole", &[State::Activated], &[0]);
        let events = diff(None, &s);
        assert_eq!(
            events,
            vec![
                Event::Title("Konsole".into()),
                Event::AppId("org.kde.konsole".into()),
                Event::OutputEnter(0),
                Event::State(vec![State::Activated]),
                Event::Done,
            ]
        );
    }

    /// The one that keeps an idle desktop idle. The compositor calls diff on
    /// every commit; a bare `done` each time would wake the dock's event loop
    /// forever for nothing.
    #[test]
    fn no_change_sends_nothing_at_all() {
        let s = snap("Konsole", "org.kde.konsole", &[State::Activated], &[0]);
        assert!(diff(Some(&s), &s).is_empty());
    }

    #[test]
    fn only_the_changed_field_is_sent() {
        let a = snap("one", "app", &[State::Activated], &[0]);
        let b = snap("two", "app", &[State::Activated], &[0]);
        assert_eq!(
            diff(Some(&a), &b),
            vec![Event::Title("two".into()), Event::Done]
        );
    }

    /// Every burst ends with exactly one Done, and nothing follows it. A client
    /// is entitled to treat the preceding events as one atomic update.
    #[test]
    fn every_burst_ends_with_exactly_one_done() {
        let a = snap("one", "app", &[], &[0]);
        let b = snap("two", "other", &[State::Maximized], &[1]);
        let events = diff(Some(&a), &b);
        assert_eq!(events.iter().filter(|e| **e == Event::Done).count(), 1);
        assert_eq!(events.last(), Some(&Event::Done));
    }

    /// Moving between outputs must never leave the window claiming both, or a
    /// dock that groups by output counts it twice.
    #[test]
    fn output_leave_precedes_output_enter() {
        let a = snap("w", "app", &[], &[0]);
        let b = snap("w", "app", &[], &[1]);
        let events = diff(Some(&a), &b);
        let leave = events.iter().position(|e| *e == Event::OutputLeave(0));
        let enter = events.iter().position(|e| *e == Event::OutputEnter(1));
        assert!(leave.is_some() && enter.is_some());
        assert!(leave < enter, "{events:?}");
    }

    /// A window spanning two outputs keeps the one it did not leave.
    #[test]
    fn a_shared_output_is_not_re_announced() {
        let a = snap("w", "app", &[], &[0, 1]);
        let b = snap("w", "app", &[], &[1, 2]);
        let events = diff(Some(&a), &b);
        assert!(events.contains(&Event::OutputLeave(0)));
        assert!(events.contains(&Event::OutputEnter(2)));
        assert!(!events.contains(&Event::OutputEnter(1)), "1 was already entered");
        assert!(!events.contains(&Event::OutputLeave(1)));
    }

    /// The state array is replaced wholesale, so a window that gains a state
    /// must re-send the ones it already had.
    #[test]
    fn state_is_sent_whole_not_as_a_delta() {
        let a = snap("w", "app", &[State::Activated], &[]);
        let b = snap("w", "app", &[State::Activated, State::Maximized], &[]);
        let events = diff(Some(&a), &b);
        assert_eq!(
            events,
            vec![
                Event::State(vec![State::Maximized, State::Activated]),
                Event::Done,
            ],
            "both states must appear, not just the new one"
        );
    }

    /// Losing every state still sends the array — an empty one. Sending
    /// nothing would leave a client believing the window is still activated.
    #[test]
    fn losing_the_last_state_sends_an_empty_array() {
        let a = snap("w", "app", &[State::Activated], &[]);
        let b = snap("w", "app", &[], &[]);
        assert_eq!(
            diff(Some(&a), &b),
            vec![Event::State(vec![]), Event::Done]
        );
    }

    /// Reordering the same outputs is not a change. The compositor's iteration
    /// order is not stable and must not generate traffic.
    #[test]
    fn output_order_is_not_a_change() {
        let a = snap("w", "app", &[], &[0, 1, 2]);
        let mut b = a.clone();
        b.outputs = [2, 0, 1].into_iter().collect();
        assert!(diff(Some(&a), &b).is_empty());
    }

    #[test]
    fn an_untitled_window_still_gets_a_label() {
        assert_eq!(snap("Konsole", "org.kde.konsole", &[], &[]).label(), "Konsole");
        assert_eq!(snap("", "org.kde.konsole", &[], &[]).label(), "org.kde.konsole");
        assert_eq!(snap("", "", &[], &[]).label(), "(untitled)");
    }

    #[test]
    fn state_predicates_read_the_set() {
        let s = snap("w", "app", &[State::Minimized], &[]);
        assert!(s.is_minimized());
        assert!(!s.is_activated());
    }
}
