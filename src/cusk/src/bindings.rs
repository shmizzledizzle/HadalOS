//! The keyboard bindings, described once.
//!
//! There were **three** lists of these, and they were free to disagree:
//! `binding_for`'s match in the compositor, the banner printed at startup, and
//! a test enumerating what the banner advertised. Each was maintained by hand.
//! A binding could be implemented and undocumented, documented and unbound, or
//! present in the test and nowhere else, and nothing would say so.
//!
//! Now the match is the executable truth and `DOCUMENTED` is the description of
//! it, with `check` tying the two together: a test walks the table and asserts
//! every documented single-key chord actually resolves. That does not prove the
//! reverse — a binding added to the match and left out of the table is still
//! possible — so `every_binding_variant_is_documented` covers that direction by
//! requiring one row per `Binding` variant.
//!
//! In the library because `cusk-keys` renders this and the compositor executes
//! it, which is the same reason `entry` is here: the alternative is a cheatsheet
//! that lies about the session it is describing.
//!
//! # What is *not* here
//!
//! `Ctrl+Alt+Escape` and `Ctrl+Alt+F1..F12` are handled by the tty driver's
//! chord table (`tty::Chorded`), before the compositor's bindings are consulted,
//! and they ignore `input.mod-key` entirely. They are documented here because a
//! shortcut list that omits how to leave the session is not a shortcut list —
//! but they are marked `Fixed`, because rendering them as "super + ctrl + alt +
//! escape" would be a lie.

use smithay::input::keyboard::{Keysym, ModifiersState};

/// Which modifier arms the compositor's bindings.
///
/// Super is correct for a real session and wrong for a nested one: KDE's
/// default `CommandAllKey` is Meta, bound to Meta+LMB move and Meta+RMB
/// resize — the same gestures — so KWin consumes them before the nested window
/// sees anything. `CUSK_MOD=alt` exists so the bindings can be exercised under
/// a host that has already claimed Super.
///
/// In the library because `cusk-keys` prints every chord as "<label> + key".
/// The label has to be the one this session actually resolved, and a client
/// reading only the config file would miss a `CUSK_MOD` override entirely — it
/// would confidently print "super" for a session running on alt.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ModKey {
    Super,
    Alt,
    Ctrl,
    CtrlAlt,
}

impl ModKey {
    /// Resolve from the config, with `CUSK_MOD` overriding it.
    ///
    /// The env var stays because it is a testing affordance for nested runs
    /// under a host that claims the same modifier, and editing a config file
    /// to try the other one is friction in exactly the wrong place.
    ///
    /// The compositor passes the variable down to the panels it spawns, so a
    /// nested session's cheatsheet agrees with its bindings.
    pub fn resolve(configured: &str) -> Self {
        let chosen = std::env::var("CUSK_MOD").unwrap_or_else(|_| configured.to_string());
        Self::parse(&chosen)
    }

    pub fn parse(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "alt" => ModKey::Alt,
            "ctrl" => ModKey::Ctrl,
            "ctrl-alt" | "ctrlalt" => ModKey::CtrlAlt,
            "" | "super" | "logo" | "meta" => ModKey::Super,
            other => {
                tracing::warn!("mod key {other:?} not recognised, using super");
                ModKey::Super
            }
        }
    }

    pub fn held(self, m: &ModifiersState) -> bool {
        match self {
            ModKey::Super => m.logo,
            ModKey::Alt => m.alt,
            ModKey::Ctrl => m.ctrl,
            ModKey::CtrlAlt => m.ctrl && m.alt,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ModKey::Super => "super",
            ModKey::Alt => "alt",
            ModKey::Ctrl => "ctrl",
            ModKey::CtrlAlt => "ctrl + alt",
        }
    }
}

/// One thing a key can ask the compositor to do.
///
/// Data, not behaviour: `apply_binding` stays in the compositor, because
/// carrying it out needs the space, the seat and the socket name. Only the
/// intent is shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Binding {
    ToggleMaximize,
    ToggleTiling,
    ToggleFloating,
    CycleLayout,
    Widen(i32),
    Spawn,
    Launcher,
    /// Show the shortcut list — this table, rendered.
    Keys,
    FocusStep(isize),
    MoveInOrder(isize),
    Promote,
    Workspace(usize),
    SendToWorkspace(usize),
}

/// Which part of the session a binding belongs to.
///
/// Ordered as the cheatsheet lists them: what you do to a window, then to the
/// arrangement, then to workspaces, then to the session itself — narrowest
/// scope first, because that is the order of how often they are wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Group {
    Windows,
    Layout,
    Workspaces,
    Session,
}

impl Group {
    pub const ALL: [Group; 4] = [Group::Windows, Group::Layout, Group::Workspaces, Group::Session];

    pub fn title(self) -> &'static str {
        match self {
            Group::Windows => "Windows",
            Group::Layout => "Layout",
            Group::Workspaces => "Workspaces",
            Group::Session => "Session",
        }
    }
}

/// How a chord is written out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chord {
    /// Armed by the configured mod key: `Mod("m")` renders as "super + m", or
    /// "alt + m" under `CUSK_MOD=alt`. Never hardcode the modifier — the whole
    /// reason this is a function of the label is that `input.mod-key` moves it.
    Mod(&'static str),
    /// A chord the mod key setting does not touch: the tty driver's exit and
    /// VT-switch chords, and the bare pointer gestures.
    Fixed(&'static str),
}

impl Chord {
    /// Render the chord for display, given whatever the mod key is called.
    pub fn render(self, mod_label: &str) -> String {
        match self {
            Chord::Mod(keys) => format!("{mod_label} + {keys}"),
            Chord::Fixed(keys) => keys.to_string(),
        }
    }
}

/// One row of the shortcut list.
pub struct Documented {
    pub group: Group,
    pub chord: Chord,
    pub description: &'static str,
    /// The keysym that produces this binding, when one key does.
    ///
    /// Present so the table can be *checked* against `resolve` instead of
    /// trusted. `None` for rows no single keysym covers — pointer gestures, the
    /// tty chords, and ranges like `1..9` — which are therefore the rows where a
    /// documentation error can still hide.
    pub check: Option<Keysym>,
}

/// Every binding, in the order the cheatsheet shows them.
pub const DOCUMENTED: &[Documented] = &[
    // ── Windows ─────────────────────────────────────────────────────────────
    Documented {
        group: Group::Windows,
        chord: Chord::Fixed("click"),
        description: "focus and raise",
        check: None,
    },
    Documented {
        group: Group::Windows,
        chord: Chord::Mod("drag"),
        description: "move the window",
        check: None,
    },
    Documented {
        group: Group::Windows,
        chord: Chord::Mod("right drag"),
        description: "resize from the nearest corner",
        check: None,
    },
    Documented {
        group: Group::Windows,
        chord: Chord::Mod("m"),
        description: "maximise / restore",
        check: Some(Keysym::m),
    },
    Documented {
        group: Group::Windows,
        chord: Chord::Mod("space"),
        description: "float this window out of the layout",
        check: Some(Keysym::space),
    },
    Documented {
        group: Group::Windows,
        chord: Chord::Mod("j / k"),
        description: "focus next / previous window",
        check: Some(Keysym::j),
    },
    Documented {
        group: Group::Windows,
        chord: Chord::Mod("shift + j / k"),
        description: "move it earlier / later in the layout",
        check: Some(Keysym::J),
    },
    Documented {
        group: Group::Windows,
        chord: Chord::Mod("shift + p"),
        description: "promote it to master",
        check: Some(Keysym::P),
    },
    // ── Layout ──────────────────────────────────────────────────────────────
    Documented {
        group: Group::Layout,
        chord: Chord::Mod("t"),
        description: "tiling on / off",
        check: Some(Keysym::t),
    },
    Documented {
        group: Group::Layout,
        chord: Chord::Mod("e"),
        description: "cycle layout (master-stack, columns)",
        check: Some(Keysym::e),
    },
    Documented {
        group: Group::Layout,
        chord: Chord::Mod("h / l"),
        description: "narrow / widen the master column",
        check: Some(Keysym::h),
    },
    // ── Workspaces ──────────────────────────────────────────────────────────
    Documented {
        group: Group::Workspaces,
        chord: Chord::Mod("1..9"),
        description: "switch workspace",
        // A range, not a key. `resolve` reads the digit out of the keysym, so
        // there is no single symbol to check here.
        check: None,
    },
    Documented {
        group: Group::Workspaces,
        chord: Chord::Mod("shift + 1..9"),
        description: "send this window to that workspace",
        check: None,
    },
    // ── Session ─────────────────────────────────────────────────────────────
    Documented {
        group: Group::Session,
        chord: Chord::Mod("enter"),
        description: "open another terminal",
        check: Some(Keysym::Return),
    },
    Documented {
        group: Group::Session,
        chord: Chord::Mod("d"),
        description: "application launcher",
        check: Some(Keysym::d),
    },
    Documented {
        group: Group::Session,
        chord: Chord::Mod("/"),
        description: "this list of shortcuts",
        check: Some(Keysym::slash),
    },
    Documented {
        group: Group::Session,
        chord: Chord::Fixed("ctrl + alt + f1..f12"),
        description: "switch to another virtual terminal",
        check: None,
    },
    Documented {
        group: Group::Session,
        chord: Chord::Fixed("ctrl + alt + escape"),
        description: "end the session",
        check: None,
    },
];

/// Which binding a keysym asks for, if any.
///
/// The executable truth. `DOCUMENTED` describes this; when they disagree, this
/// is what the session does.
pub fn resolve(sym: Keysym, shift: bool) -> Option<Binding> {
    match sym {
        Keysym::m => Some(Binding::ToggleMaximize),
        Keysym::t => Some(Binding::ToggleTiling),
        Keysym::space => Some(Binding::ToggleFloating),
        Keysym::e => Some(Binding::CycleLayout),
        Keysym::l => Some(Binding::Widen(1)),
        Keysym::h => Some(Binding::Widen(-1)),
        Keysym::Return | Keysym::KP_Enter => Some(Binding::Spawn),
        Keysym::d => Some(Binding::Launcher),
        // Both, because the shifted form of `/` is `?` on most layouts and
        // someone reaching for "help" presses whichever their fingers find.
        // Matching only `slash` made Shift+/ silently do nothing.
        Keysym::slash | Keysym::question => Some(Binding::Keys),
        Keysym::j => Some(Binding::FocusStep(1)),
        Keysym::k => Some(Binding::FocusStep(-1)),
        // Shift gives the capitalised keysym, so the
        // shifted bindings are distinguished here
        // rather than by re-reading modifier state.
        Keysym::J => Some(Binding::MoveInOrder(1)),
        Keysym::K => Some(Binding::MoveInOrder(-1)),
        Keysym::P => Some(Binding::Promote),
        // Digits pick a workspace; shifted digits send
        // the focused window to one. Shift produces a
        // different keysym per layout (! " # on some,
        // symbols on others), so the unshifted keysym
        // is read and the modifier checked separately —
        // matching on the shifted symbol works on one
        // keyboard layout and silently fails on the
        // rest.
        sym => match sym.raw() {
            0x0031..=0x0039 => {
                let index = (sym.raw() - 0x0031) as usize;
                Some(if shift {
                    Binding::SendToWorkspace(index)
                } else {
                    Binding::Workspace(index)
                })
            }
            _ => None,
        },
    }
}

/// The rows of one group, in table order.
pub fn in_group(group: Group) -> impl Iterator<Item = &'static Documented> {
    DOCUMENTED.iter().filter(move |row| row.group == group)
}

/// The whole list, grouped and rendered, ready to print or draw.
///
/// One function rather than each caller walking `DOCUMENTED` itself: the banner
/// and the cheatsheet differ in how they *lay out* the rows, not in which rows
/// there are or what the chords say.
pub fn rendered(mod_label: &str) -> Vec<(Group, Vec<(String, &'static str)>)> {
    Group::ALL
        .iter()
        .map(|&group| {
            let rows = in_group(group)
                .map(|row| (row.chord.render(mod_label), row.description))
                .collect();
            (group, rows)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The point of `check`. A documented chord that resolves to nothing is a
    /// shortcut list advertising a dead key, which is worse than not listing it.
    #[test]
    fn every_documented_chord_resolves() {
        for row in DOCUMENTED {
            let Some(sym) = row.check else { continue };
            assert!(
                resolve(sym, false).is_some(),
                "{:?} is documented as {:?} but resolves to nothing",
                sym,
                row.chord,
            );
        }
    }

    /// The other direction: a binding implemented and never documented. Checked
    /// by variant rather than by keysym, because that is what a reader of the
    /// cheatsheet notices — a thing the session can do that the list omits.
    #[test]
    fn every_binding_variant_is_documented() {
        // Constructed by hand precisely because `Binding` cannot be enumerated.
        // Adding a variant and forgetting this list fails to compile — the
        // match below is exhaustive with no wildcard, which is the mechanism.
        let described = |binding: Binding| -> &'static str {
            match binding {
                Binding::ToggleMaximize => "maximise / restore",
                Binding::ToggleTiling => "tiling on / off",
                Binding::ToggleFloating => "float this window out of the layout",
                Binding::CycleLayout => "cycle layout (master-stack, columns)",
                Binding::Widen(_) => "narrow / widen the master column",
                Binding::Spawn => "open another terminal",
                Binding::Launcher => "application launcher",
                Binding::Keys => "this list of shortcuts",
                Binding::FocusStep(_) => "focus next / previous window",
                Binding::MoveInOrder(_) => "move it earlier / later in the layout",
                Binding::Promote => "promote it to master",
                Binding::Workspace(_) => "switch workspace",
                Binding::SendToWorkspace(_) => "send this window to that workspace",
            }
        };

        for binding in [
            Binding::ToggleMaximize,
            Binding::ToggleTiling,
            Binding::ToggleFloating,
            Binding::CycleLayout,
            Binding::Widen(1),
            Binding::Spawn,
            Binding::Launcher,
            Binding::Keys,
            Binding::FocusStep(1),
            Binding::MoveInOrder(1),
            Binding::Promote,
            Binding::Workspace(0),
            Binding::SendToWorkspace(0),
        ] {
            let wanted = described(binding);
            assert!(
                DOCUMENTED.iter().any(|row| row.description == wanted),
                "{binding:?} is implemented but no row describes it",
            );
        }
    }

    /// An unbound key must resolve to nothing, or ordinary typing disappears
    /// whenever the modifier happens to be down.
    #[test]
    fn unbound_keys_are_not_claimed() {
        for sym in [Keysym::a, Keysym::z, Keysym::F5, Keysym::Escape, Keysym::_0] {
            assert_eq!(resolve(sym, false), None, "{sym:?} was claimed");
        }
    }

    #[test]
    fn digits_pick_a_workspace_and_shift_sends_to_it() {
        assert_eq!(resolve(Keysym::_1, false), Some(Binding::Workspace(0)));
        assert_eq!(resolve(Keysym::_9, false), Some(Binding::Workspace(8)));
        assert_eq!(resolve(Keysym::_1, true), Some(Binding::SendToWorkspace(0)));
        assert_eq!(resolve(Keysym::_3, true), Some(Binding::SendToWorkspace(2)));
    }

    /// Either half of the key. Matching only `slash` meant Shift+/ — which is
    /// how `?` is typed, and what someone reaching for help actually presses —
    /// did nothing at all.
    #[test]
    fn both_halves_of_the_help_key_work() {
        assert_eq!(resolve(Keysym::slash, false), Some(Binding::Keys));
        assert_eq!(resolve(Keysym::question, true), Some(Binding::Keys));
    }

    /// The modifier is rendered, never hardcoded: `input.mod-key` moves every
    /// chord in the table, and a list that said "super" under `CUSK_MOD=alt`
    /// would be confidently wrong about every line.
    #[test]
    fn the_modifier_is_rendered_not_assumed() {
        assert_eq!(Chord::Mod("m").render("super"), "super + m");
        assert_eq!(Chord::Mod("m").render("alt"), "alt + m");
        // The tty chords do not move.
        assert_eq!(
            Chord::Fixed("ctrl + alt + escape").render("alt"),
            "ctrl + alt + escape"
        );
    }

    /// Every group must have something in it, or the cheatsheet draws a heading
    /// over nothing.
    #[test]
    fn no_group_is_empty() {
        for group in Group::ALL {
            assert!(in_group(group).count() > 0, "{group:?} has no bindings");
        }
        assert_eq!(
            rendered("super").iter().map(|(_, rows)| rows.len()).sum::<usize>(),
            DOCUMENTED.len(),
            "grouping lost or duplicated a row"
        );
    }
}
