//! The typed configuration schema.
//!
//! `docs/cusk.md` §4: "One typed schema. Every setting has a type, a default, a
//! range and a description. The GUI is generated from it; the parser validates
//! against it; documentation is generated from it. **There is no second list to
//! keep in sync.**"
//!
//! That last sentence is the whole design constraint, and it is why this file
//! opens with a macro instead of a struct. Declaring the settings twice — once
//! as struct fields and once as a descriptor table — is the obvious
//! implementation and it decays immediately: a setting added to one and not the
//! other produces a key the parser accepts and nothing reads, or a field the
//! GUI cannot see. The macro makes that class of mistake unrepresentable,
//! because there is only one place to add a setting.
//!
//! One declaration generates:
//!
//! - the `Config` struct, with a real Rust type per field
//! - `Default`, from the declared defaults
//! - `SCHEMA`, the descriptor table the GUI and the docs will read
//! - `get` / `set` by key, which is the surface Hadal proposes changes through
//!
//! # Why TOML
//!
//! §4 requires the GUI to be "a round-tripping editor" that "keeps comments,
//! blank lines and ordering". `toml_edit` is a syntax-tree editor built for
//! precisely that, so the failure §4 names — "a GUI that eats your comments is
//! a GUI nobody uses twice" — is designed out rather than defended against. A
//! bespoke Hyprland-style format would mean writing that tree by hand, which is
//! the actual work, not the syntax.

use std::fmt;
use std::path::Path;

use toml_edit::{DocumentMut, Item};

/// A configuration value, in the four shapes the schema supports.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i32),
    Float(f64),
    Bool(bool),
    Text(String),
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Value::Int(v) => write!(f, "{v}"),
            Value::Float(v) => write!(f, "{v}"),
            Value::Bool(v) => write!(f, "{v}"),
            Value::Text(v) => write!(f, "{v}"),
        }
    }
}

/// What a setting accepts: its type, its default, and its bounds.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Int { default: i32, min: i32, max: i32 },
    Float { default: f64, min: f64, max: f64 },
    Bool { default: bool },
    /// Free text — a path, a command. Unvalidated beyond being a string,
    /// because the set of valid values is not knowable here: a wallpaper that
    /// does not exist yet is a normal thing to have in a config you are still
    /// writing, and refusing it would be the editor arguing with the user.
    Text { default: &'static str },
    /// A closed set of names. Modelled separately from `Text` because the GUI
    /// renders it as a picker and the validator can reject a typo, neither of
    /// which is possible for free text.
    Choice { default: &'static str, options: &'static [&'static str] },
}

impl Kind {
    pub fn type_name(&self) -> &'static str {
        match self {
            Kind::Int { .. } => "integer",
            Kind::Float { .. } => "number",
            Kind::Bool { .. } => "boolean",
            Kind::Text { .. } => "text",
            Kind::Choice { .. } => "choice",
        }
    }

    pub fn default_value(&self) -> Value {
        match self {
            Kind::Int { default, .. } => Value::Int(*default),
            Kind::Float { default, .. } => Value::Float(*default),
            Kind::Bool { default } => Value::Bool(*default),
            Kind::Text { default } => Value::Text((*default).to_string()),
            Kind::Choice { default, .. } => Value::Text((*default).to_string()),
        }
    }

    /// Human-readable bounds, for the GUI and for error messages.
    pub fn range(&self) -> Option<String> {
        match self {
            Kind::Int { min, max, .. } => Some(format!("{min} to {max}")),
            Kind::Float { min, max, .. } => Some(format!("{min} to {max}")),
            Kind::Bool { .. } => None,
            Kind::Text { .. } => None,
            Kind::Choice { options, .. } => Some(options.join(", ")),
        }
    }

    /// Check a value against this setting, returning why it does not fit.
    ///
    /// Rejects rather than clamps. Clamping an out-of-range value means the
    /// file says one thing and the compositor does another, with nothing to
    /// tell the user which — the same silent divergence §4 objects to in
    /// GUI-versus-file.
    fn check(&self, value: &Value) -> Result<Value, Problem> {
        let mismatch = || Problem::WrongType {
            expected: self.type_name(),
            got: match value {
                Value::Int(_) => "integer",
                Value::Float(_) => "number",
                Value::Bool(_) => "boolean",
                Value::Text(_) => "string",
            },
        };
        match (self, value) {
            (Kind::Int { min, max, .. }, Value::Int(v)) => {
                if v < min || v > max {
                    Err(Problem::OutOfRange { allowed: self.range().unwrap_or_default() })
                } else {
                    Ok(value.clone())
                }
            }
            // An integer where a number is wanted is a widening, not a type
            // error. Requiring 1.0 in a file where 1 is the obvious thing to
            // write is a papercut with no upside.
            (Kind::Float { .. }, Value::Int(v)) => self.check(&Value::Float(*v as f64)),
            (Kind::Float { min, max, .. }, Value::Float(v)) => {
                if v < min || v > max {
                    Err(Problem::OutOfRange { allowed: self.range().unwrap_or_default() })
                } else {
                    Ok(Value::Float(*v))
                }
            }
            (Kind::Bool { .. }, Value::Bool(_)) => Ok(value.clone()),
            (Kind::Text { .. }, Value::Text(_)) => Ok(value.clone()),
            (Kind::Choice { options, .. }, Value::Text(v)) => {
                if options.contains(&v.as_str()) {
                    Ok(value.clone())
                } else {
                    Err(Problem::OutOfRange { allowed: options.join(", ") })
                }
            }
            _ => Err(mismatch()),
        }
    }
}

/// When a change to a setting takes effect.
///
/// Part of the schema rather than tribal knowledge, because the alternative is
/// the worst outcome hot reload can have: the user edits a setting, nothing
/// happens, and nothing says why. The GUI can render this, and reload reports
/// it by name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Apply {
    /// Takes effect the moment the file is saved.
    Live,
    /// Describes initial state, so reapplying it would fight the user.
    /// `layout.default` is what a workspace *starts* as; clobbering a layout
    /// chosen since with Super+E would make the keybinding feel broken.
    Restart,
}

/// One setting: everything the parser, the GUI and the docs need to know.
#[derive(Debug, Clone, Copy)]
pub struct Setting {
    /// Dotted path, matching the TOML structure: `section.name`.
    pub key: &'static str,
    pub kind: Kind,
    pub doc: &'static str,
    pub apply: Apply,
}

impl Setting {
    /// Split the dotted key into its TOML table and leaf.
    fn path(&self) -> (&'static str, &'static str) {
        match self.key.split_once('.') {
            Some((table, leaf)) => (table, leaf),
            None => ("", self.key),
        }
    }
}

/// Why a value was not accepted.
#[derive(Debug, Clone, PartialEq)]
pub enum Problem {
    UnknownKey,
    WrongType { expected: &'static str, got: &'static str },
    OutOfRange { allowed: String },
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Problem::UnknownKey => write!(f, "no such setting"),
            Problem::WrongType { expected, got } => {
                write!(f, "expected {expected}, found {got}")
            }
            Problem::OutOfRange { allowed } => write!(f, "allowed: {allowed}"),
        }
    }
}

/// A problem, attached to the key it concerns.
#[derive(Debug, Clone, PartialEq)]
pub struct Complaint {
    pub key: String,
    pub problem: Problem,
}

impl fmt::Display for Complaint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.key, self.problem)
    }
}

/// Maps a `Kind` variant to the Rust type its field holds, so the declaration
/// below states the type exactly once.
macro_rules! rust_type {
    (Int) => { i32 };
    (Float) => { f64 };
    (Bool) => { bool };
    (Text) => { String };
    (Choice) => { String };
}

macro_rules! from_value {
    (Int, $v:expr) => { match $v { Value::Int(x) => x, _ => unreachable!() } };
    (Float, $v:expr) => { match $v { Value::Float(x) => x, _ => unreachable!() } };
    (Bool, $v:expr) => { match $v { Value::Bool(x) => x, _ => unreachable!() } };
    (Text, $v:expr) => { match $v { Value::Text(x) => x, _ => unreachable!() } };
    (Choice, $v:expr) => { match $v { Value::Text(x) => x, _ => unreachable!() } };
}

macro_rules! to_value {
    (Int, $v:expr) => { Value::Int($v) };
    (Float, $v:expr) => { Value::Float($v) };
    (Bool, $v:expr) => { Value::Bool($v) };
    (Text, $v:expr) => { Value::Text($v.clone()) };
    (Choice, $v:expr) => { Value::Text($v.clone()) };
}

macro_rules! settings {
    ($(
        $field:ident : $kind:ident {
            key: $key:literal, doc: $doc:literal, apply: $apply:ident, $($rest:tt)*
        }
    ),* $(,)?) => {
        /// Every setting cusk understands.
        ///
        /// Generated from the same declaration as `Config`, so a setting cannot
        /// exist in one and not the other.
        pub const SCHEMA: &[Setting] = &[
            $(Setting {
                key: $key,
                kind: Kind::$kind { $($rest)* },
                doc: $doc,
                apply: Apply::$apply,
            }),*
        ];

        #[derive(Debug, Clone, PartialEq)]
        pub struct Config {
            $(pub $field: rust_type!($kind)),*
        }

        impl Default for Config {
            fn default() -> Self {
                Self {
                    $($field: from_value!($kind, Kind::$kind { $($rest)* }.default_value())),*
                }
            }
        }

        impl Config {
            pub fn get(&self, key: &str) -> Option<Value> {
                match key {
                    $($key => Some(to_value!($kind, self.$field)),)*
                    _ => None,
                }
            }

            /// Validate and apply one setting.
            ///
            /// This is the surface §4 has Hadal propose changes through: a
            /// `SetSetting { key, value }` the broker range-checks against the
            /// schema before anything is written.
            pub fn set(&mut self, key: &str, value: Value) -> Result<(), Problem> {
                match key {
                    $($key => {
                        let checked = Kind::$kind { $($rest)* }.check(&value)?;
                        self.$field = from_value!($kind, checked);
                        Ok(())
                    })*
                    _ => Err(Problem::UnknownKey),
                }
            }
        }
    };
}

settings! {
    inner_gap: Int {
        key: "layout.inner-gap",
        doc: "Space between tiles, in pixels.",
        apply: Live,
        default: 8, min: 0, max: 200
    },
    outer_gap: Int {
        key: "layout.outer-gap",
        doc: "Space between tiles and the screen edge, in pixels.",
        apply: Live,
        default: 8, min: 0, max: 200
    },
    master_ratio: Float {
        key: "layout.master-ratio",
        doc: "Fraction of the width taken by the master column.",
        apply: Live,
        default: 0.6, min: 0.1, max: 0.9
    },
    default_layout: Choice {
        key: "layout.default",
        doc: "Which arrangement tiled workspaces start in.",
        apply: Restart,
        default: "master-stack", options: &["master-stack", "columns"]
    },
    tiling_on_start: Bool {
        key: "layout.tile-by-default",
        doc: "Whether new workspaces tile rather than float.",
        apply: Restart,
        default: false
    },
    workspace_count: Int {
        key: "workspaces.count",
        doc: "How many workspaces exist.",
        apply: Live,
        default: 4, min: 1, max: 9
    },
    mod_key: Choice {
        key: "input.mod-key",
        doc: "The modifier that arms compositor bindings.",
        apply: Live,
        default: "super", options: &["super", "alt", "ctrl", "ctrl-alt"]
    },
    focus_follows_mouse: Bool {
        key: "input.focus-follows-mouse",
        doc: "Whether hovering a window focuses it, without a click.",
        apply: Live,
        default: false
    },
    wallpaper: Text {
        key: "appearance.wallpaper",
        doc: "Image shown behind windows. Leave empty for a plain background.",
        apply: Live,
        default: ""
    },
    blur: Bool {
        key: "appearance.blur",
        doc: "Blur the wallpaper where it shows through a window.",
        apply: Live,
        default: true
    },
    blur_radius: Int {
        key: "appearance.blur-radius",
        doc: "How far the blur spreads, in pixels.",
        apply: Live,
        default: 24, min: 0, max: 120
    },
    font: Text {
        key: "appearance.font",
        doc: "Path to a font file. Empty finds one on the system.",
        apply: Restart,
        default: ""
    },
    panel_height: Int {
        key: "appearance.panel-height",
        doc: "Height of the workspace bar along the top edge. Zero hides it.",
        apply: Live,
        default: 28, min: 0, max: 64
    },
    window_opacity: Float {
        key: "appearance.window-opacity",
        doc: "How opaque windows are. Below 1.0 the blur behind them becomes visible.",
        apply: Live,
        default: 1.0, min: 0.2, max: 1.0
    },
    window_blur: Bool {
        key: "appearance.window-blur",
        doc: "Blur what is actually behind a window, not just the wallpaper. Costs a blur per window per frame.",
        apply: Live,
        default: false
    },
    corner_radius: Int {
        key: "appearance.corner-radius",
        doc: "Rounding of window corners, in pixels.",
        apply: Live,
        default: 12, min: 0, max: 32
    },
    ring_width: Int {
        key: "appearance.focus-ring",
        doc: "Thickness of the ring around the focused window. Zero hides it.",
        apply: Live,
        default: 2, min: 0, max: 8
    },
    blur_passes: Int {
        key: "appearance.blur-passes",
        doc: "Box-blur passes. Three approximates a Gaussian closely.",
        apply: Live,
        default: 3, min: 1, max: 5
    },
    launcher: Text {
        key: "commands.launcher",
        doc: "Program the launcher binding runs. A bare name is looked for beside cusk, then on PATH.",
        apply: Live,
        default: "cusk-launcher"
    },
    terminal: Choice {
        key: "commands.terminal",
        doc: "Terminal opened by the spawn binding.",
        apply: Live,
        default: "auto", options: &["auto", "foot", "alacritty", "kitty", "weston-terminal", "konsole"]
    },
}

/// Terminals cusk knows how to launch, in preference order.
///
/// Read from the schema rather than kept beside it. A separate `TERMINALS`
/// constant is exactly the second list this module exists to prevent: one that
/// accepts a name the launcher will not run, or runs one the validator
/// rejects.
pub fn known_terminals() -> impl Iterator<Item = &'static str> {
    match Config::setting("commands.terminal").map(|s| s.kind) {
        Some(Kind::Choice { options, .. }) => options.iter().copied(),
        _ => [].iter().copied(),
    }
    .filter(|name| *name != "auto")
}

/// Where the configuration lives.
pub fn default_path() -> std::path::PathBuf {
    let base = std::env::var("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            std::path::PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config")
        });
    base.join("cusk").join("cusk.toml")
}

impl Config {
    pub fn setting(key: &str) -> Option<&'static Setting> {
        SCHEMA.iter().find(|s| s.key == key)
    }

    /// Read a document, applying what is valid and reporting what is not.
    ///
    /// Deliberately not all-or-nothing. A compositor that refuses to start
    /// because of one bad line leaves the user with no desktop to fix it from;
    /// one that silently reverts the line gives them no idea why their setting
    /// did nothing. §4 asks for errors "surfaced rather than silently reverting
    /// to defaults", so unusable values fall back to the default *and* are
    /// returned.
    pub fn from_document(doc: &DocumentMut) -> (Self, Vec<Complaint>) {
        let mut config = Config::default();
        let mut complaints = Vec::new();

        for setting in SCHEMA {
            let (table, leaf) = setting.path();
            let item = match doc.get(table) {
                Some(Item::Table(t)) => t.get(leaf),
                _ => None,
            };
            let Some(item) = item else { continue };
            let Some(value) = item_to_value(item) else {
                complaints.push(Complaint {
                    key: setting.key.to_string(),
                    problem: Problem::WrongType {
                        expected: setting.kind.type_name(),
                        got: "an unsupported type",
                    },
                });
                continue;
            };
            if let Err(problem) = config.set(setting.key, value) {
                complaints.push(Complaint { key: setting.key.to_string(), problem });
            }
        }

        // Keys nobody reads are reported too. A misspelt setting that is
        // silently ignored is the single most common configuration complaint
        // there is, and the file itself gives no hint.
        for (table_name, table) in doc.iter() {
            let Item::Table(table) = table else { continue };
            for (leaf, _) in table.iter() {
                let key = format!("{table_name}.{leaf}");
                if Config::setting(&key).is_none() {
                    complaints.push(Complaint { key, problem: Problem::UnknownKey });
                }
            }
        }

        (config, complaints)
    }

    pub fn from_str(text: &str) -> Result<(Self, Vec<Complaint>), toml_edit::TomlError> {
        Ok(Self::from_document(&text.parse::<DocumentMut>()?))
    }

    pub fn load(path: &Path) -> Result<(Self, Vec<Complaint>), Box<dyn std::error::Error>> {
        Ok(Self::from_str(&std::fs::read_to_string(path)?)?)
    }

    /// A commented file documenting every setting at its default.
    ///
    /// Generated from the schema, per §4's "documentation is generated from
    /// it" — a hand-written sample file is the second list this module exists
    /// to avoid, and it goes stale the first time a default changes.
    pub fn default_file() -> String {
        let mut out = String::from(
            "# cusk configuration\n\
             #\n\
             # Every setting below is shown at its default and commented out.\n\
             # Uncomment a line to change it. Comments and ordering are preserved\n\
             # when the settings GUI writes to this file.\n",
        );
        let mut current = "";
        for setting in SCHEMA {
            let (table, leaf) = setting.path();
            if table != current {
                out.push_str(&format!("\n[{table}]\n"));
                current = table;
            }
            out.push_str(&format!("\n# {}\n", setting.doc));
            if setting.apply == Apply::Restart {
                out.push_str("# takes effect on restart\n");
            }
            if let Some(range) = setting.kind.range() {
                out.push_str(&format!("# {}: {range}\n", setting.kind.type_name()));
            }
            let default = setting.kind.default_value();
            let rendered = match default {
                Value::Text(ref s) => format!("\"{s}\""),
                ref v => v.to_string(),
            };
            out.push_str(&format!("# {leaf} = {rendered}\n"));
        }
        out
    }
}

fn item_to_value(item: &Item) -> Option<Value> {
    let value = item.as_value()?;
    if let Some(v) = value.as_integer() {
        return Some(Value::Int(v as i32));
    }
    if let Some(v) = value.as_float() {
        return Some(Value::Float(v));
    }
    if let Some(v) = value.as_bool() {
        return Some(Value::Bool(v));
    }
    if let Some(v) = value.as_str() {
        return Some(Value::Text(v.to_string()));
    }
    None
}

/// Write one setting into a document, leaving everything else exactly as it was.
///
/// This is the round-tripping half of §4. The GUI edits one node; comments,
/// blank lines and ordering survive because the document is a syntax tree and
/// not a deserialise-then-reserialise round trip.
pub fn set_in_document(
    doc: &mut DocumentMut,
    key: &str,
    value: Value,
) -> Result<(), Problem> {
    let setting = Config::setting(key).ok_or(Problem::UnknownKey)?;
    let checked = setting.kind.check(&value)?;
    let (table, leaf) = setting.path();

    if doc.get(table).is_none() {
        let mut new = toml_edit::Table::new();
        // Implicit tables do not print a [header], which would produce a file
        // whose keys sit under whatever section happens to precede them.
        new.set_implicit(false);
        doc.insert(table, Item::Table(new));
    }
    let Some(Item::Table(table)) = doc.get_mut(table) else {
        return Err(Problem::UnknownKey);
    };

    let mut new = match checked {
        Value::Int(v) => toml_edit::Value::from(v as i64),
        Value::Float(v) => toml_edit::Value::from(v),
        Value::Bool(v) => toml_edit::Value::from(v),
        Value::Text(v) => toml_edit::Value::from(v),
    };

    // Carry the old node's decor onto the new one. A value's decor is where
    // toml_edit keeps the whitespace and the trailing `# comment`, so a plain
    // assignment writes the right number and silently eats the note the user
    // left beside it — the exact failure §4 says makes a GUI unusable.
    if let Some(existing) = table.get(leaf).and_then(|i| i.as_value()) {
        *new.decor_mut() = existing.decor().clone();
    }
    table[leaf] = Item::Value(new);
    Ok(())
}


/// The sections settings are grouped under, in the order the schema declares
/// them.
///
/// First-appearance order rather than alphabetical: the schema's order is a
/// deliberate editorial choice about what a reader meets first, and sorting it
/// would throw that away and put "commands" before "layout".
pub fn sections() -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for setting in SCHEMA {
        let (table, _) = setting.path();
        if !out.contains(&table) {
            out.push(table);
        }
    }
    out
}

/// The settings in one section, in schema order.
pub fn settings_in(section: &str) -> Vec<&'static Setting> {
    SCHEMA.iter().filter(|s| s.path().0 == section).collect()
}

/// The leaf name, formatted for display: `master-ratio` becomes `Master ratio`.
pub fn label_of(setting: &Setting) -> String {
    let (_, leaf) = setting.path();
    let mut chars = leaf.replace('-', " ");
    if let Some(first) = chars.get_mut(0..1) {
        first.make_ascii_uppercase();
    }
    chars
}

// ── hot reload ───────────────────────────────────────────────────────────

/// A cheap fingerprint of the file, for detecting edits.
///
/// Modification time alone is not enough: it has one-second granularity on
/// some filesystems, and two saves inside the same second are exactly what
/// happens when someone is iterating on a setting. Length and inode make the
/// common cases distinguishable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Stamp {
    modified: Option<std::time::SystemTime>,
    len: u64,
    inode: u64,
}

impl Stamp {
    fn of(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(path).ok()?;
        Some(Stamp {
            modified: meta.modified().ok(),
            len: meta.len(),
            inode: meta.ino(),
        })
    }
}

/// What a reload attempt produced.
#[derive(Debug)]
pub enum Reload {
    /// The file has not changed since the last look.
    Unchanged,
    /// A new configuration, with anything the file got wrong.
    ///
    /// Which settings need a restart is deliberately *not* reported here: the
    /// watcher does not know what is currently running, and a field it could
    /// only ever fill with a guess is a second source of truth. Callers ask
    /// `restart_only_changes` with the config they actually hold.
    Applied { config: Config, complaints: Vec<Complaint> },
    /// The file could not be parsed. The previous configuration stands.
    Failed(String),
}

/// Watches the configuration file and re-reads it when it changes.
///
/// Polls rather than using inotify, which is not laziness. Editors overwhelmingly
/// save by writing a temporary file and renaming it over the target, which
/// replaces the inode and silently kills a watch registered on the old one —
/// the reload works once and then never again, which is worse than not having
/// it. Watching the directory instead works but means handling every unrelated
/// event in it. Re-stating one path costs nothing at this interval and cannot
/// be fooled by a rename.
pub struct Watcher {
    path: std::path::PathBuf,
    stamp: Option<Stamp>,
    last_checked: std::time::Instant,
    interval: std::time::Duration,
}

impl Watcher {
    pub fn new(path: std::path::PathBuf) -> Self {
        Self {
            stamp: Stamp::of(&path),
            path,
            last_checked: std::time::Instant::now(),
            interval: std::time::Duration::from_millis(500),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Re-read the file if it has changed since the last call.
    ///
    /// Safe to call every frame; the rate limit is inside.
    pub fn poll(&mut self) -> Reload {
        if self.last_checked.elapsed() < self.interval {
            return Reload::Unchanged;
        }
        self.last_checked = std::time::Instant::now();
        self.check_now()
    }

    /// Accept the file as it is now, without reporting it as a change.
    ///
    /// For a program that writes the file it is watching. Without this the
    /// settings GUI reads back its own save as an external edit and rebuilds
    /// its state from disk mid-interaction, which lands on top of whatever the
    /// user is currently dragging.
    pub fn resync(&mut self) {
        self.stamp = Stamp::of(&self.path);
    }

    /// The same check without the rate limit, so tests do not sleep.
    pub fn check_now(&mut self) -> Reload {
        let stamp = Stamp::of(&self.path);
        if stamp == self.stamp {
            return Reload::Unchanged;
        }
        // A deleted file is not an instruction to reset the desktop to
        // defaults. Keep what is running and wait for it to come back — this
        // is also the brief window a rename-based save passes through.
        let Some(stamp) = stamp else {
            return Reload::Unchanged;
        };
        self.stamp = Some(stamp);

        let text = match std::fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(e) => return Reload::Failed(e.to_string()),
        };
        match Config::from_str(&text) {
            Ok((config, complaints)) => Reload::Applied { config, complaints },
            // Crucially not a fallback to defaults. Editors save partial files,
            // and resetting every setting because a half-written line was
            // flushed to disk would tear the desktop apart mid-edit.
            Err(e) => Reload::Failed(e.to_string()),
        }
    }
}

/// Settings that differ between two configurations and only apply at startup.
pub fn restart_only_changes(old: &Config, new: &Config) -> Vec<&'static str> {
    SCHEMA
        .iter()
        .filter(|s| s.apply == Apply::Restart)
        .filter(|s| old.get(s.key) != new.get(s.key))
        .map(|s| s.key)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A default outside its own range is a typo that would otherwise surface
    /// as a setting nobody can restore, since resetting writes an invalid
    /// value straight back.
    #[test]
    fn every_default_satisfies_its_own_schema() {
        for setting in SCHEMA {
            let default = setting.kind.default_value();
            assert!(
                setting.kind.check(&default).is_ok(),
                "{} default {default} violates its own constraint",
                setting.key
            );
        }
    }

    /// The launcher and the validator must agree on the same list, or a name
    /// the file accepts is a name nothing will start.
    #[test]
    fn the_terminal_list_comes_only_from_the_schema() {
        let names: Vec<&str> = known_terminals().collect();
        assert!(!names.is_empty());
        assert!(!names.contains(&"auto"), "auto is a strategy, not a program");
        let mut config = Config::default();
        for name in names {
            assert!(
                config.set("commands.terminal", Value::Text(name.into())).is_ok(),
                "{name} is launchable but not configurable"
            );
        }
    }

    #[test]
    fn keys_are_unique() {
        let mut seen = std::collections::HashSet::new();
        for setting in SCHEMA {
            assert!(seen.insert(setting.key), "duplicate key {}", setting.key);
        }
    }

    /// The GUI groups by table and the parser splits on the dot, so a key
    /// without a section would land at the document root where neither looks.
    #[test]
    fn every_key_is_sectioned_and_lowercase() {
        for setting in SCHEMA {
            assert!(setting.key.contains('.'), "{} has no section", setting.key);
            assert_eq!(
                setting.key,
                setting.key.to_lowercase(),
                "{} is not lowercase",
                setting.key
            );
            assert!(!setting.doc.is_empty(), "{} has no description", setting.key);
        }
    }

    /// The generated struct and the descriptor table must not drift. If they
    /// could, this file would have failed at its one job.
    #[test]
    fn every_schema_entry_is_readable_and_writable() {
        let mut config = Config::default();
        for setting in SCHEMA {
            let value = config.get(setting.key);
            assert_eq!(
                value.as_ref(),
                Some(&setting.kind.default_value()),
                "{} reads back wrong",
                setting.key
            );
            assert!(
                config.set(setting.key, setting.kind.default_value()).is_ok(),
                "{} rejects its own default",
                setting.key
            );
        }
    }

    #[test]
    fn out_of_range_values_are_rejected_not_clamped() {
        let mut config = Config::default();
        assert!(matches!(
            config.set("layout.inner-gap", Value::Int(9999)),
            Err(Problem::OutOfRange { .. })
        ));
        assert_eq!(config.inner_gap, 8, "the old value must survive a rejection");
    }

    #[test]
    fn a_typo_in_a_choice_is_rejected() {
        let mut config = Config::default();
        let err = config.set("layout.default", Value::Text("mastersstack".into()));
        assert!(matches!(err, Err(Problem::OutOfRange { .. })));
    }

    #[test]
    fn wrong_types_are_rejected() {
        let mut config = Config::default();
        assert!(matches!(
            config.set("layout.inner-gap", Value::Bool(true)),
            Err(Problem::WrongType { .. })
        ));
    }

    /// Writing 1 where 1.0 is wanted is not a mistake worth an error.
    ///
    /// Checked against `Kind` directly rather than through a setting: no
    /// integer falls inside master-ratio's 0.1..=0.9, which is what makes this
    /// property invisible from the current schema.
    #[test]
    fn an_integer_is_accepted_for_a_number() {
        let kind = Kind::Float { default: 1.0, min: 0.0, max: 10.0 };
        assert_eq!(kind.check(&Value::Int(3)), Ok(Value::Float(3.0)));
        assert!(
            matches!(kind.check(&Value::Int(11)), Err(Problem::OutOfRange { .. })),
            "widening must not skip the range check"
        );
    }

    /// Widening must not extend to the reverse: a fractional value in an
    /// integer setting has no correct rounding, and picking one silently would
    /// mean the file and the compositor disagree.
    #[test]
    fn a_fraction_is_not_accepted_for_an_integer() {
        let mut config = Config::default();
        assert!(matches!(
            config.set("layout.inner-gap", Value::Float(4.5)),
            Err(Problem::WrongType { .. })
        ));
    }

    #[test]
    fn unknown_keys_are_reported_rather_than_ignored() {
        let (_, complaints) =
            Config::from_str("[layout]\ninner-gpa = 4\n").unwrap();
        assert_eq!(
            complaints,
            vec![Complaint { key: "layout.inner-gpa".into(), problem: Problem::UnknownKey }]
        );
    }

    /// One bad line must not take the whole file with it, or a typo leaves the
    /// user with no desktop to fix it from.
    #[test]
    fn a_bad_value_costs_only_its_own_setting() {
        let (config, complaints) = Config::from_str(
            "[layout]\ninner-gap = 20\nouter-gap = 100000\nmaster-ratio = 0.75\n",
        )
        .unwrap();
        assert_eq!(config.inner_gap, 20, "valid settings before it still apply");
        assert_eq!(config.master_ratio, 0.75, "and after it");
        assert_eq!(config.outer_gap, 8, "the bad one falls back to its default");
        assert_eq!(complaints.len(), 1);
        assert_eq!(complaints[0].key, "layout.outer-gap");
    }

    /// §4: "A GUI that eats your comments is a GUI nobody uses twice."
    #[test]
    fn editing_a_value_preserves_comments_and_ordering() {
        let original = "\
# my cusk config
# hands off the ordering

[layout]
# I like a wide master
master-ratio = 0.7

inner-gap = 4   # tight

[input]
mod-key = \"alt\"
";
        let mut doc = original.parse::<DocumentMut>().unwrap();
        set_in_document(&mut doc, "layout.inner-gap", Value::Int(12)).unwrap();
        let written = doc.to_string();

        assert!(written.contains("# my cusk config"));
        assert!(written.contains("# hands off the ordering"));
        assert!(written.contains("# I like a wide master"));
        assert!(written.contains("# tight"), "trailing comment survives");
        assert!(written.contains("inner-gap = 12"));
        assert!(
            written.find("master-ratio").unwrap() < written.find("inner-gap").unwrap(),
            "ordering must not be normalised"
        );
        let (config, complaints) = Config::from_str(&written).unwrap();
        assert!(complaints.is_empty());
        assert_eq!(config.inner_gap, 12);
        assert_eq!(config.master_ratio, 0.7);
        assert_eq!(config.mod_key, "alt");
    }

    #[test]
    fn writing_into_an_absent_section_creates_it() {
        let mut doc = "".parse::<DocumentMut>().unwrap();
        set_in_document(&mut doc, "input.mod-key", Value::Text("ctrl".into())).unwrap();
        let written = doc.to_string();
        assert!(written.contains("[input]"), "section header must be written: {written}");
        let (config, complaints) = Config::from_str(&written).unwrap();
        assert!(complaints.is_empty());
        assert_eq!(config.mod_key, "ctrl");
    }

    #[test]
    fn the_document_writer_validates_too() {
        let mut doc = "".parse::<DocumentMut>().unwrap();
        assert!(matches!(
            set_in_document(&mut doc, "layout.master-ratio", Value::Float(4.0)),
            Err(Problem::OutOfRange { .. })
        ));
        assert!(matches!(
            set_in_document(&mut doc, "layout.nonesuch", Value::Int(1)),
            Err(Problem::UnknownKey)
        ));
        assert_eq!(doc.to_string(), "", "a rejected write leaves no trace");
    }

    /// The generated sample file must parse cleanly and mean what it says.
    #[test]
    fn the_generated_default_file_round_trips() {
        let text = Config::default_file();
        let (config, complaints) = Config::from_str(&text).unwrap();
        assert!(complaints.is_empty(), "{complaints:?}");
        assert_eq!(config, Config::default(), "commented-out file must be all defaults");
        for setting in SCHEMA {
            assert!(text.contains(setting.doc), "{} is undocumented", setting.key);
            let (_, leaf) = setting.path();
            assert!(text.contains(leaf), "{} is missing", setting.key);
        }
    }

    /// Uncommenting the generated file must produce exactly the defaults, or
    /// the documentation is describing a config that does not exist.
    #[test]
    fn uncommenting_the_default_file_changes_nothing() {
        let leaves: Vec<&str> = SCHEMA.iter().map(|s| s.path().1).collect();
        let text: String = Config::default_file()
            .lines()
            .map(|line| match line.strip_prefix("# ") {
                // Only the setting lines, not the prose around them — the
                // header is a comment on purpose and is not valid TOML.
                Some(rest) if leaves.iter().any(|leaf| rest.starts_with(&format!("{leaf} ="))) => rest,
                _ => line,
            })
            .collect::<Vec<_>>()
            .join("\n");
        let (config, complaints) = Config::from_str(&text).unwrap();
        assert!(complaints.is_empty(), "{complaints:?}");
        assert_eq!(config, Config::default());
    }

    fn temp_config(name: &str, body: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join("cusk-config-tests");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        path
    }

    #[test]
    fn an_unchanged_file_is_not_re_read() {
        let path = temp_config("unchanged.toml", "[layout]\ninner-gap = 3\n");
        let mut watcher = Watcher::new(path);
        assert!(matches!(watcher.check_now(), Reload::Unchanged));
    }

    #[test]
    fn an_edit_is_picked_up() {
        let path = temp_config("edited.toml", "[layout]\ninner-gap = 3\n");
        let mut watcher = Watcher::new(path.clone());
        std::fs::write(&path, "[layout]\ninner-gap = 30\n").unwrap();
        match watcher.check_now() {
            Reload::Applied { config, complaints } => {
                assert_eq!(config.inner_gap, 30);
                assert!(complaints.is_empty());
            }
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    /// Editors save by writing a temporary file and renaming it over the
    /// target. A watch on the inode dies at that moment; re-stating the path
    /// does not.
    #[test]
    fn a_rename_over_the_file_is_still_seen() {
        let path = temp_config("renamed.toml", "[layout]\ninner-gap = 3\n");
        let mut watcher = Watcher::new(path.clone());
        let temp = path.with_extension("tmp");
        std::fs::write(&temp, "[layout]\ninner-gap = 44\n").unwrap();
        std::fs::rename(&temp, &path).unwrap();
        match watcher.check_now() {
            Reload::Applied { config, .. } => assert_eq!(config.inner_gap, 44),
            other => panic!("expected Applied, got {other:?}"),
        }
    }

    /// The one that matters most. A half-written file must not reset the
    /// desktop to defaults — editors flush partial saves, and a torn-down
    /// session mid-edit is unrecoverable from inside the session.
    #[test]
    fn a_broken_file_leaves_the_running_config_alone() {
        let path = temp_config("broken.toml", "[layout]\ninner-gap = 3\n");
        let mut watcher = Watcher::new(path.clone());
        std::fs::write(&path, "[layout]\ninner-gap = ").unwrap();
        match watcher.check_now() {
            Reload::Failed(message) => assert!(!message.is_empty()),
            other => panic!("a syntax error must not produce {other:?}"),
        }
    }

    /// A file that vanishes for an instant during a rename-based save must not
    /// be read as "reset everything".
    #[test]
    fn a_missing_file_changes_nothing() {
        let path = temp_config("vanishing.toml", "[layout]\ninner-gap = 3\n");
        let mut watcher = Watcher::new(path.clone());
        std::fs::remove_file(&path).unwrap();
        assert!(matches!(watcher.check_now(), Reload::Unchanged));
    }

    /// A program that writes the file it watches must not see its own save.
    #[test]
    fn resync_swallows_our_own_write() {
        let path = temp_config("selfwrite.toml", "[layout]\ninner-gap = 3\n");
        let mut watcher = Watcher::new(path.clone());
        std::fs::write(&path, "[layout]\ninner-gap = 9\n").unwrap();
        watcher.resync();
        assert!(
            matches!(watcher.check_now(), Reload::Unchanged),
            "our own write must not come back as an external edit"
        );
    }

    #[test]
    fn restart_only_settings_are_named_not_silently_skipped() {
        let old = Config::default();
        let mut new = Config::default();
        new.inner_gap = 40;
        assert!(restart_only_changes(&old, &new).is_empty(), "a live setting needs no restart");

        new.default_layout = "columns".into();
        new.tiling_on_start = true;
        let named = restart_only_changes(&old, &new);
        assert!(named.contains(&"layout.default"));
        assert!(named.contains(&"layout.tile-by-default"));
    }

    /// Every setting has to declare when it takes effect, or the reporting
    /// above has a hole in it exactly where a user would be confused.
    #[test]
    fn the_generated_file_says_when_each_setting_applies() {
        let text = Config::default_file();
        for setting in SCHEMA.iter().filter(|s| s.apply == Apply::Restart) {
            assert!(
                text.contains("restart"),
                "{} applies only at restart and the file does not say so",
                setting.key
            );
        }
    }

    #[test]
    fn sections_are_in_schema_order_and_unique() {
        let sections = sections();
        assert_eq!(sections, vec!["layout", "workspaces", "input", "appearance", "commands"]);
        let unique: std::collections::HashSet<_> = sections.iter().collect();
        assert_eq!(unique.len(), sections.len());
    }

    /// Every setting must reach exactly one section, or the GUI silently drops
    /// it — a setting that exists, validates, and is unreachable from the
    /// editor is worse than one that is missing.
    #[test]
    fn every_setting_appears_in_exactly_one_section() {
        let mut seen = 0;
        for section in sections() {
            seen += settings_in(section).len();
        }
        assert_eq!(seen, SCHEMA.len());
    }

    #[test]
    fn labels_are_readable() {
        let setting = Config::setting("layout.master-ratio").unwrap();
        assert_eq!(label_of(setting), "Master ratio");
    }
    /// Does editing the *generated* file produce a file that still parses?
    ///
    /// The generated file's tables are empty, with every setting commented
    /// out. Writing into one is the normal path — the settings GUI does
    /// nothing else — so if that can produce a duplicate key or a second table
    /// header, every user's config eventually stops parsing.
    #[test]
    fn editing_the_generated_file_keeps_it_valid() {
        let mut doc = Config::default_file().parse::<DocumentMut>().unwrap();
        for (key, value) in [
            ("layout.inner-gap", Value::Int(35)),
            ("layout.master-ratio", Value::Float(0.42)),
            ("appearance.window-blur", Value::Bool(true)),
            ("appearance.window-opacity", Value::Float(0.85)),
            // Twice, because a GUI writes the same key on every drag.
            ("appearance.window-blur", Value::Bool(true)),
            ("commands.launcher", Value::Text("/somewhere/cusk-launcher".into())),
        ] {
            set_in_document(&mut doc, key, value).unwrap();
        }
        let written = doc.to_string();
        let (config, complaints) = Config::from_str(&written)
            .unwrap_or_else(|e| panic!("the edited file no longer parses: {e}\n{written}"));
        assert!(complaints.is_empty(), "{complaints:?}");
        assert_eq!(config.inner_gap, 35);
        assert!(config.window_blur);

        // Real keys only — the generated file also has a *commented* copy of
        // every setting, and counting those would make this pass or fail for
        // the wrong reason.
        let real = |needle: &str| {
            written
                .lines()
                .filter(|line| !line.trim_start().starts_with('#') && line.trim_start().starts_with(needle))
                .count()
        };
        assert_eq!(real("window-blur"), 1, "duplicate key:\n{written}");
        assert_eq!(real("[commands]"), 1, "duplicate table:\n{written}");
    }
}
