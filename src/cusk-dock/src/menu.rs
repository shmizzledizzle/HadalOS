//! Tray context menus, over `com.canonical.dbusmenu`.
//!
//! Right-clicking a tray icon is, for most applications, the *primary* way to
//! use it — quit, preferences, "open at login", the device list. An icon that
//! only answers left clicks is a status light.
//!
//! # There is no menu in StatusNotifierItem
//!
//! SNI carries a `Menu` property, and its value is an **object path to a second,
//! unrelated protocol**: `com.canonical.dbusmenu`, which is a whole tree
//! description with its own revision counter, its own event channel, and its own
//! notion of what a menu item is. So supporting right-click is not "read one
//! more property" — it is speaking a second protocol to a second object on the
//! same connection.
//!
//! # The layout is nested variants, and that is where the bugs live
//!
//! `GetLayout` returns `(u32 revision, (i32 id, a{sv} properties, av children))`
//! — and each element of `children` is a **variant wrapping another such
//! structure**, recursively. Rust's type system cannot describe that shape
//! usefully, so it arrives as `Value` and has to be walked by hand.
//!
//! Everything in this module is therefore a pure function over `Value`, so the
//! walk can be tested against hand-built trees with no bus, no application, and
//! no tray. That matters more here than anywhere else in the dock: a
//! mis-decoded menu is not a blank space, it is *the wrong item at the position
//! the user clicked* — the failure mode where "Quit" and "Preferences" swap
//! places, which is unacceptable in a way that a missing icon is not.
//!
//! # Defaults are specified, and getting them wrong inverts behaviour
//!
//! `enabled` and `visible` default to **true** when absent, and most
//! applications omit them for ordinary items. Reading a missing key as `false`
//! yields an empty, greyed-out menu from a perfectly good application — which
//! looks like the menu failed to load rather than like a decoding bug.

use zbus::zvariant::{OwnedValue, Value};

/// What kind of row this is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Standard,
    /// A divider. Carries no label and cannot be clicked.
    Separator,
}

/// How a row shows its on/off state, if it has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    None,
    Checkmark,
    Radio,
}

/// One row of a menu, and its submenu if it has one.
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// The id to send back in `Event`. Not an index: ids are the application's
    /// own and are neither contiguous nor ordered, so a click must carry this
    /// rather than a position in the list.
    pub id: i32,
    pub label: String,
    pub kind: Kind,
    pub enabled: bool,
    pub toggle: Toggle,
    /// `Some(true)` for on, `Some(false)` for off, `None` for a row that has no
    /// toggle or reports itself indeterminate.
    pub checked: Option<bool>,
    /// A theme icon name, resolved the same way desktop entries are.
    pub icon_name: Option<String>,
    /// Rows of the submenu. Empty for a leaf.
    pub children: Vec<Entry>,
    /// Whether the application says this row *has* a submenu.
    ///
    /// Distinct from `children` being non-empty: an application may declare
    /// `children-display = "submenu"` and return nothing until `AboutToShow`
    /// has been called. Drawing no arrow in that case makes a menu look like it
    /// has no submenus, and the user never opens the one place the interesting
    /// options live.
    pub has_submenu: bool,
}

impl Entry {
    /// Whether clicking this row should do anything.
    ///
    /// Separators and disabled rows are drawn but inert. A separator that can
    /// be clicked sends an `Event` for an id the application does not consider
    /// actionable, and the results range from nothing to a crash in the
    /// application's own handler.
    pub fn clickable(&self) -> bool {
        self.kind == Kind::Standard && self.enabled && !self.has_submenu
    }
}

/// Read a `bool` property, with the spec's default when it is absent.
///
/// The default is the substance. `enabled` and `visible` are both `true` by
/// default and are both routinely omitted, so treating absence as `false`
/// produces an empty greyed-out menu from a working application.
fn flag(properties: &[(String, OwnedValue)], key: &str, default: bool) -> bool {
    properties
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| bool::try_from(value).ok())
        .unwrap_or(default)
}

fn string(properties: &[(String, OwnedValue)], key: &str) -> Option<String> {
    properties
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| <&str>::try_from(value).ok().map(str::to_string))
        .filter(|s| !s.is_empty())
}

fn integer(properties: &[(String, OwnedValue)], key: &str) -> Option<i32> {
    properties
        .iter()
        .find(|(name, _)| name == key)
        .and_then(|(_, value)| i32::try_from(value).ok())
}

/// Strip the mnemonic marker from a label.
///
/// dbusmenu labels carry `_` before the accelerator character, as in
/// `_Preferences`. There is no keyboard navigation in this menu, so the marker
/// is noise — but it has to be *removed* rather than ignored, or every second
/// item reads as though it were misspelled. `__` is a literal underscore.
pub fn strip_mnemonic(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut chars = label.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '_' {
            if chars.peek() == Some(&'_') {
                chars.next();
                out.push('_');
            }
            // A single underscore marks the next character; drop the marker and
            // keep the character.
            continue;
        }
        out.push(c);
    }
    out
}

/// Unwrap any number of `Value::Value` layers.
///
/// The wire signature `(ia{sv}av)` says the dictionary and the child array are
/// *not* variant-wrapped, but the elements of `av` are — and some
/// implementations wrap more than the signature requires. Peeling
/// unconditionally at every level costs one pointer chase and accepts both
/// shapes, where matching `Value::Dict` directly silently produced an empty menu
/// from a perfectly well-formed tree.
///
/// Found by probing what `Structure::from` actually builds rather than by
/// reading the specification: the specification describes the wire, and this has
/// to survive whatever the sender's binding did on top of it.
fn peel<'v>(mut value: &'v Value<'v>) -> &'v Value<'v> {
    while let Value::Value(inner) = value {
        value = inner;
    }
    value
}

/// Turn a `a{sv}` value into pairs, so the property lookups above can be plain
/// slice searches rather than repeated dictionary conversions.
fn properties_of(value: &Value<'_>) -> Vec<(String, OwnedValue)> {
    let Value::Dict(dict) = peel(value) else { return Vec::new() };
    dict.iter()
        .filter_map(|(key, value)| {
            let key = <&str>::try_from(peel(key)).ok()?;
            // Peeled before storing, not at each read. `a{sv}` values are
            // variants by definition, and `bool::try_from` on a variant-wrapped
            // bool fails rather than unwrapping — so `enabled` and `visible`
            // silently fell back to their defaults and every disabled row drew
            // as enabled. `<&str>::try_from` *does* peel, which is why the
            // labels worked and hid the bug.
            let owned = OwnedValue::try_from(peel(value).try_clone().ok()?).ok()?;
            Some((key.to_string(), owned))
        })
        .collect()
}

/// Walk one `(i32, a{sv}, av)` node and everything under it.
///
/// Returns `None` for a node that is malformed or marked invisible. Invisible
/// rows are dropped here rather than filtered later, so no caller has to
/// remember to: an application that hides a row and finds it drawn anyway has
/// been actively contradicted, which is worse than a menu that is merely
/// incomplete.
fn walk(value: &Value<'_>, depth: usize) -> Option<Entry> {
    // A cycle in the tree, or an application reporting absurd nesting, must not
    // recurse until the stack runs out. Twelve is far past any real menu and
    // still cheap.
    if depth > 12 {
        return None;
    }
    // Children are `av`, so each arrives variant-wrapped; peel before matching.
    let Value::Structure(node) = peel(value) else { return None };
    let fields = node.fields();
    if fields.len() < 3 {
        return None;
    }

    let id = i32::try_from(peel(&fields[0])).ok()?;
    let properties = properties_of(&fields[1]);

    if !flag(&properties, "visible", true) {
        return None;
    }

    let kind = match string(&properties, "type").as_deref() {
        Some("separator") => Kind::Separator,
        // Unknown types are treated as standard rather than dropped. The spec
        // allows applications to invent them, and an unrecognised row with a
        // label is still worth showing.
        _ => Kind::Standard,
    };

    let toggle = match string(&properties, "toggle-type").as_deref() {
        Some("checkmark") => Toggle::Checkmark,
        Some("radio") => Toggle::Radio,
        _ => Toggle::None,
    };

    // 0 is off, 1 is on, and anything else — the spec says -1 — means
    // indeterminate. Mapping indeterminate to "off" would draw a definite
    // answer the application declined to give.
    let checked = match toggle {
        Toggle::None => None,
        _ => match integer(&properties, "toggle-state") {
            Some(1) => Some(true),
            Some(0) => Some(false),
            _ => None,
        },
    };

    let has_submenu = string(&properties, "children-display").as_deref() == Some("submenu");

    let children = match peel(&fields[2]) {
        Value::Array(array) => array
            .iter()
            .filter_map(|child| walk(child, depth + 1))
            .collect(),
        _ => Vec::new(),
    };

    Some(Entry {
        id,
        label: string(&properties, "label").map(|l| strip_mnemonic(&l)).unwrap_or_default(),
        kind,
        enabled: flag(&properties, "enabled", true),
        toggle,
        checked,
        icon_name: string(&properties, "icon-name"),
        has_submenu: has_submenu || !children.is_empty(),
        children,
    })
}

/// Parse a `GetLayout` reply into the rows of the top-level menu.
///
/// The root node is a container, not a row — it has an id (usually 0) and no
/// label — so its *children* are the menu. Returning the root itself would draw
/// a menu with one blank item that opens the real one.
pub fn parse_layout(root: &Value<'_>) -> Vec<Entry> {
    match walk(root, 0) {
        Some(entry) => entry.children,
        None => Vec::new(),
    }
}

/// Flatten a tree into rows to draw, with a depth per row.
///
/// Submenus are drawn inline and indented rather than as flyouts. A flyout needs
/// pointer-tracking, a grab, and a decision about which edge to open towards
/// when the menu is against the screen edge — none of which the dock has, and
/// all of which fail visibly when wrong. Indentation is honest about the
/// structure and cannot open off-screen.
///
/// Only *expanded* submenus are descended into, so a long menu does not arrive
/// fully unfolded.
pub fn rows(entries: &[Entry], expanded: &[i32], depth: usize) -> Vec<(Entry, usize)> {
    let mut out = Vec::new();
    for entry in entries {
        out.push((entry.clone(), depth));
        if entry.has_submenu && expanded.contains(&entry.id) {
            out.extend(rows(&entry.children, expanded, depth + 1));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use zbus::zvariant::Dict;

    /// Build a `(i32, a{sv}, av)` node the way `GetLayout` returns one.
    fn node(
        id: i32,
        properties: &[(&'static str, Value<'static>)],
        children: Vec<Value<'static>>,
    ) -> Value<'static> {
        let mut dict: Dict<'static, 'static> = Dict::new(
            &zbus::zvariant::Signature::Str,
            &zbus::zvariant::Signature::Variant,
        );
        for (key, value) in properties {
            dict.append(Value::Str((*key).into()), Value::Value(Box::new(value.try_clone().unwrap())))
                .unwrap();
        }
        let children: Vec<Value<'static>> = children
            .into_iter()
            .map(|c| Value::Value(Box::new(c)))
            .collect();
        Value::Structure(
            zbus::zvariant::Structure::from((
                id,
                Value::Dict(dict),
                Value::Array(children.into()),
            )),
        )
    }

    fn label(text: &str) -> (&'static str, Value<'static>) {
        // Leaked deliberately and only in tests: the helper needs a 'static key
        // and the alternative is threading lifetimes through every case.
        ("label", Value::Str(text.to_string().into()))
    }

    #[test]
    fn the_root_is_a_container_and_its_children_are_the_menu() {
        let tree = node(
            0,
            &[],
            vec![
                node(1, &[label("Preferences")], vec![]),
                node(2, &[label("Quit")], vec![]),
            ],
        );
        let menu = parse_layout(&tree);
        assert_eq!(menu.len(), 2, "the root itself must not become a row");
        assert_eq!(menu[0].label, "Preferences");
        assert_eq!(menu[1].id, 2);
    }

    /// The defaults that invert behaviour when read wrong. Most applications
    /// omit both keys for ordinary items, so absence must mean enabled and
    /// visible — otherwise a working application yields an empty greyed menu.
    #[test]
    fn absent_enabled_and_visible_default_to_true() {
        let tree = node(0, &[], vec![node(1, &[label("Quit")], vec![])]);
        let menu = parse_layout(&tree);
        assert!(menu[0].enabled, "a row with no `enabled` key must be enabled");
        assert!(menu[0].clickable());
    }

    /// A present `false` must be distinguishable from an absent key, and this
    /// is the test that caught the bug the defaults were hiding.
    ///
    /// `a{sv}` values are variants, and `bool::try_from` on a variant-wrapped
    /// bool *fails* rather than unwrapping — so every `enabled: false` and
    /// `visible: false` fell through to its default of `true`. Because
    /// `<&str>::try_from` does peel, labels decoded correctly and the menu
    /// looked entirely right: every row drawn, every row clickable, nothing
    /// greyed and nothing hidden. The defaults being *correct* is what made the
    /// decoding bug invisible.
    #[test]
    fn a_present_false_is_not_confused_with_an_absent_key() {
        let tree = node(
            0,
            &[],
            vec![
                node(1, &[label("Absent")], vec![]),
                node(2, &[label("Explicit"), ("enabled", Value::Bool(false))], vec![]),
            ],
        );
        let menu = parse_layout(&tree);
        assert!(menu[0].enabled, "absent means enabled");
        assert!(
            !menu[1].enabled,
            "an explicit false must survive the variant unwrapping"
        );
    }

    #[test]
    fn an_invisible_row_is_dropped_entirely() {
        let tree = node(
            0,
            &[],
            vec![
                node(1, &[label("Shown")], vec![]),
                node(2, &[label("Hidden"), ("visible", Value::Bool(false))], vec![]),
            ],
        );
        let menu = parse_layout(&tree);
        assert_eq!(menu.len(), 1);
        assert_eq!(menu[0].label, "Shown");
    }

    #[test]
    fn a_disabled_row_is_kept_but_not_clickable() {
        let tree = node(
            0,
            &[],
            vec![node(1, &[label("Busy"), ("enabled", Value::Bool(false))], vec![])],
        );
        let menu = parse_layout(&tree);
        assert_eq!(menu.len(), 1, "disabled is drawn, unlike invisible");
        assert!(!menu[0].enabled);
        assert!(!menu[0].clickable());
    }

    #[test]
    fn separators_are_never_clickable() {
        let tree = node(
            0,
            &[],
            vec![node(1, &[("type", Value::Str("separator".into()))], vec![])],
        );
        let menu = parse_layout(&tree);
        assert_eq!(menu[0].kind, Kind::Separator);
        assert!(
            !menu[0].clickable(),
            "clicking a separator sends an Event for an id the application \
             does not consider actionable"
        );
    }

    /// 0 off, 1 on, anything else indeterminate. Mapping indeterminate to off
    /// would draw a definite answer the application declined to give.
    #[test]
    fn toggle_state_distinguishes_off_from_indeterminate() {
        let build = |state: i32| {
            node(
                0,
                &[],
                vec![node(
                    1,
                    &[
                        label("Mute"),
                        ("toggle-type", Value::Str("checkmark".into())),
                        ("toggle-state", Value::I32(state)),
                    ],
                    vec![],
                )],
            )
        };
        assert_eq!(parse_layout(&build(1))[0].checked, Some(true));
        assert_eq!(parse_layout(&build(0))[0].checked, Some(false));
        assert_eq!(parse_layout(&build(-1))[0].checked, None, "indeterminate");
    }

    /// A row with no toggle must report no state, not "off" — otherwise every
    /// ordinary menu item draws an empty checkbox.
    #[test]
    fn a_row_without_a_toggle_has_no_check_state() {
        let tree = node(0, &[], vec![node(1, &[label("Quit")], vec![])]);
        assert_eq!(parse_layout(&tree)[0].checked, None);
        assert_eq!(parse_layout(&tree)[0].toggle, Toggle::None);
    }

    #[test]
    fn nested_submenus_are_parsed() {
        let tree = node(
            0,
            &[],
            vec![node(
                1,
                &[label("Devices"), ("children-display", Value::Str("submenu".into()))],
                vec![node(2, &[label("Headset")], vec![])],
            )],
        );
        let menu = parse_layout(&tree);
        assert!(menu[0].has_submenu);
        assert_eq!(menu[0].children.len(), 1);
        assert_eq!(menu[0].children[0].label, "Headset");
        assert!(
            !menu[0].clickable(),
            "a row that opens a submenu must not also send a click"
        );
    }

    /// An application may declare a submenu and return nothing until
    /// `AboutToShow`. Drawing no arrow makes the menu look like it has no
    /// submenus at all.
    #[test]
    fn a_declared_but_empty_submenu_still_reports_one() {
        let tree = node(
            0,
            &[],
            vec![node(
                1,
                &[label("Devices"), ("children-display", Value::Str("submenu".into()))],
                vec![],
            )],
        );
        let menu = parse_layout(&tree);
        assert!(menu[0].has_submenu);
        assert!(menu[0].children.is_empty());
    }

    /// `_Preferences` must read as "Preferences", not as a typo. `__` is a
    /// literal underscore.
    #[test]
    fn mnemonic_markers_are_stripped() {
        assert_eq!(strip_mnemonic("_Preferences"), "Preferences");
        assert_eq!(strip_mnemonic("Save _As"), "Save As");
        assert_eq!(strip_mnemonic("Wi__Fi"), "Wi_Fi");
        assert_eq!(strip_mnemonic("Quit"), "Quit");
    }

    /// Malformed input must produce an empty menu rather than a panic: this
    /// data comes from another process and is not to be trusted.
    #[test]
    fn malformed_layouts_yield_nothing() {
        assert!(parse_layout(&Value::I32(7)).is_empty());
        assert!(parse_layout(&Value::Str("nonsense".into())).is_empty());
        // A structure with too few fields.
        let short = Value::Structure(zbus::zvariant::Structure::from((1i32,)));
        assert!(parse_layout(&short).is_empty());
    }

    /// Submenus stay folded until asked for, or a menu with several submenus
    /// arrives fully unfolded and taller than the screen.
    #[test]
    fn rows_descend_only_into_expanded_submenus() {
        let tree = node(
            0,
            &[],
            vec![
                node(
                    1,
                    &[label("Devices"), ("children-display", Value::Str("submenu".into()))],
                    vec![node(2, &[label("Headset")], vec![])],
                ),
                node(3, &[label("Quit")], vec![]),
            ],
        );
        let menu = parse_layout(&tree);

        let folded = rows(&menu, &[], 0);
        assert_eq!(folded.len(), 2, "the submenu's contents must stay hidden");

        let opened = rows(&menu, &[1], 0);
        assert_eq!(opened.len(), 3);
        assert_eq!(opened[1].0.label, "Headset");
        assert_eq!(opened[1].1, 1, "a submenu row is indented one level");
        assert_eq!(opened[2].0.label, "Quit", "siblings still follow it");
    }

    /// Depth is bounded, so a cyclic or absurdly nested tree cannot exhaust the
    /// stack. This is untrusted input from another process.
    #[test]
    fn absurd_nesting_is_refused_rather_than_recursed() {
        let mut tree = node(99, &[label("deep")], vec![]);
        for id in 0..40 {
            tree = node(id, &[label("nest")], vec![tree]);
        }
        // The point is that this returns rather than overflowing the stack.
        let menu = parse_layout(&tree);
        let flattened = rows(&menu, &(0..40).collect::<Vec<_>>(), 0);
        assert!(flattened.len() < 20, "depth must be capped");
    }

    /// The exact shape a real application returned, captured from the wire.
    ///
    /// `kdeconnect-indicator`, read with
    /// `busctl call :1.x /MenuBar com.canonical.dbusmenu GetLayout iias 0 -- -1 0`:
    ///
    /// ```text
    /// u(ia{sv}av) 2 0 1 "children-display" s "submenu"
    ///                 1 (ia{sv}av) 1 1 "label" s "Open app" 0
    /// ```
    ///
    /// Note what it does *not* contain: no `type`, no `enabled`, no `visible`,
    /// no `toggle-type`. A real menu is mostly absent keys, which is why the
    /// defaults are the part most worth testing — this row is enabled and
    /// visible entirely by omission.
    ///
    /// Note also the **root** carries `children-display: submenu`. Treating the
    /// root as a row would draw one item called "" that opens the real menu.
    #[test]
    fn a_real_applications_layout_decodes() {
        let tree = node(
            0,
            &[("children-display", Value::Str("submenu".into()))],
            vec![node(1, &[label("Open app")], vec![])],
        );
        let menu = parse_layout(&tree);

        assert_eq!(menu.len(), 1, "the root must not become a row");
        assert_eq!(menu[0].id, 1);
        assert_eq!(menu[0].label, "Open app");
        assert_eq!(menu[0].kind, Kind::Standard);
        assert!(menu[0].enabled, "enabled by omission");
        assert_eq!(menu[0].toggle, Toggle::None);
        assert_eq!(menu[0].checked, None);
        assert!(!menu[0].has_submenu, "a leaf, despite the root having one");
        assert!(menu[0].clickable());
    }

    /// Ids are the application's own and need not be contiguous or ordered. A
    /// click has to carry the id, not the row's position.
    #[test]
    fn ids_are_preserved_verbatim() {
        let tree = node(
            0,
            &[],
            vec![
                node(904, &[label("First")], vec![]),
                node(12, &[label("Second")], vec![]),
            ],
        );
        let menu = parse_layout(&tree);
        assert_eq!(menu[0].id, 904);
        assert_eq!(menu[1].id, 12);
    }

    /// The dict conversion has to survive properties it does not know, since
    /// applications ship plenty.
    #[test]
    fn unknown_properties_are_ignored_not_fatal() {
        let tree = node(
            0,
            &[],
            vec![node(
                1,
                &[
                    label("Quit"),
                    ("x-canonical-something", Value::U32(3)),
                    ("accessible-desc", Value::Str("quit the app".into())),
                ],
                vec![],
            )],
        );
        let menu = parse_layout(&tree);
        assert_eq!(menu[0].label, "Quit");
        assert!(menu[0].clickable());
    }

    // Silences the unused-import warning in builds where the helper above does
    // not need it.
    #[allow(dead_code)]
    fn _hashmap_is_used(_: HashMap<String, OwnedValue>) {}
}

