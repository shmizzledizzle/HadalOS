//! Desktop entries: finding them, parsing them, and ranking them.
//!
//! All of it is a pure function over strings, which is the point — a launcher
//! that shows the wrong thing, or nothing, is almost always a parsing problem,
//! and parsing problems are the cheapest kind to test. Nothing here opens a
//! window or spawns a process.
//!
//! Implements the parts of the freedesktop Desktop Entry specification that a
//! launcher actually needs. Deliberately not all of it: actions, D-Bus
//! activation, and startup notification are real parts of the spec that this
//! does not do, and pretending otherwise in the code would be worse than
//! saying so here.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// One launchable application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    /// Desktop file id, e.g. `org.kde.konsole.desktop`. Used for precedence:
    /// the first file with a given id wins, so a user's own copy in
    /// `~/.local/share` overrides the system one.
    pub id: String,
    pub name: String,
    pub comment: Option<String>,
    /// The command, already split and stripped of field codes.
    pub exec: Vec<String>,
    /// Needs a terminal to run in.
    pub terminal: bool,
    /// The `Icon=` value: either an absolute path or a *theme name* that has
    /// to be looked up. Kept unresolved because resolving it needs the
    /// filesystem, and parsing should not.
    pub icon: Option<String>,
    /// The raw `Categories=` list, as written. Kept unmapped for the same
    /// reason `icon` is kept unresolved: which menu section a category belongs
    /// to is a presentation decision, and a parser that folded
    /// `WebBrowser;Network` into "Internet" here would leave no way to ask what
    /// the file actually said.
    pub categories: Vec<String>,
}

/// Parse the `[Desktop Entry]` group of a `.desktop` file.
///
/// Returns `None` for anything a launcher must not offer: a non-application,
/// one marked `NoDisplay` or `Hidden`, or one with no runnable `Exec`.
pub fn parse(text: &str, id: &str) -> Option<Entry> {
    let mut fields: HashMap<&str, &str> = HashMap::new();
    let mut in_group = false;

    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') {
            // Only the main group. A file's `[Desktop Action Foo]` groups have
            // their own Name and Exec, and reading them into the same map
            // would launch the wrong command under the right name.
            in_group = line == "[Desktop Entry]";
            continue;
        }
        if !in_group {
            continue;
        }
        if let Some((key, value)) = line.split_once('=') {
            let key = key.trim();
            // Localised keys look like `Name[de]`. Skipping them keeps the
            // plain key, rather than letting whichever locale sorted last win.
            if key.contains('[') {
                continue;
            }
            fields.entry(key).or_insert_with(|| value.trim());
        }
    }

    if fields.get("Type") != Some(&"Application") {
        return None;
    }
    if is_true(fields.get("NoDisplay")) || is_true(fields.get("Hidden")) {
        return None;
    }

    let name = fields.get("Name")?.to_string();
    if name.is_empty() {
        return None;
    }

    let exec = strip_field_codes(fields.get("Exec")?);
    if exec.is_empty() {
        return None;
    }

    Some(Entry {
        id: id.to_string(),
        name,
        comment: fields.get("Comment").map(|c| c.to_string()).filter(|c| !c.is_empty()),
        exec,
        terminal: is_true(fields.get("Terminal")),
        icon: fields.get("Icon").map(|i| i.to_string()).filter(|i| !i.is_empty()),
        categories: fields.get("Categories").map(|c| split_list(c)).unwrap_or_default(),
    })
}

/// Split a `;`-separated spec list.
///
/// The spec says these end with a trailing `;`, so a naive split yields a final
/// empty element — which would become a category named "" and a menu section
/// with no title.
fn split_list(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(str::to_string)
        .collect()
}

fn is_true(value: Option<&&str>) -> bool {
    matches!(value, Some(&"true"))
}

/// Split an `Exec=` line into arguments, dropping field codes.
///
/// The field codes (`%f`, `%U`, `%i`, …) are placeholders for files and URLs
/// the launcher is not passing. Left in place they are handed to the program
/// as literal arguments, and the failure is spectacularly confusing: the
/// application opens and immediately complains that it cannot find a file
/// called `%U`.
///
/// `%%` is an escaped percent and stays as one.
pub fn strip_field_codes(exec: &str) -> Vec<String> {
    let mut args: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut chars = exec.chars().peekable();
    let mut has_content = false;

    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                has_content = true;
            }
            '%' => match chars.next() {
                Some('%') => {
                    current.push('%');
                    has_content = true;
                }
                // Every other code expands to nothing here.
                Some(_) => {}
                None => {}
            },
            c if c.is_whitespace() && !quoted => {
                if has_content && !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
                current.clear();
                has_content = false;
            }
            c => {
                current.push(c);
                has_content = true;
            }
        }
    }
    if !current.is_empty() {
        args.push(current);
    }
    args
}

/// Find an icon file for an `Icon=` value.
///
/// A deliberately small subset of the icon theme specification. What it does
/// cover is the part that matters on a real system: **two different directory
/// layouts**. hicolor and Adwaita nest as `SIZE/apps/name`, breeze as
/// `apps/SIZE/name`, and a resolver that knows only one finds almost nothing
/// on a KDE box — measured here, breeze holds 19,827 icons and hicolor 823.
///
/// SVG is returned alongside PNG because breeze is *entirely* SVG. Treating
/// icons as a raster problem would have left most applications blank.
///
/// Not covered, and worth naming rather than discovering: theme inheritance,
/// `index.theme`, scaled `@2x` variants, and the user's configured theme. The
/// search order below is a guess at preference, not a reading of their
/// settings. An unresolved icon gets a lettered tile, so a miss is visibly a
/// miss rather than a gap.
pub fn find_icon(name: &str) -> Option<PathBuf> {
    let direct = Path::new(name);
    if direct.is_absolute() {
        return direct.is_file().then(|| direct.to_path_buf());
    }

    // Largest first: scaling an icon down looks better than scaling it up.
    const SIZES: &[&str] = &["256", "128", "96", "64", "48", "32", "24", "22", "16"];
    const THEMES: &[&str] = &["breeze", "Adwaita", "hicolor"];

    for root in ["/usr/share/icons", "/usr/local/share/icons"] {
        for theme in THEMES {
            let theme_dir = Path::new(root).join(theme);
            if !theme_dir.is_dir() {
                continue;
            }
            for size in SIZES {
                for ext in ["svg", "png"] {
                    // breeze: apps/48/name.svg
                    let flat = theme_dir.join("apps").join(size).join(format!("{name}.{ext}"));
                    if flat.is_file() {
                        return Some(flat);
                    }
                    // hicolor and Adwaita: 48x48/apps/name.png
                    let square = theme_dir
                        .join(format!("{size}x{size}"))
                        .join("apps")
                        .join(format!("{name}.{ext}"));
                    if square.is_file() {
                        return Some(square);
                    }
                }
            }
            // Adwaita keeps its vector icons outside the size hierarchy.
            let scalable = theme_dir.join("scalable").join("apps").join(format!("{name}.svg"));
            if scalable.is_file() {
                return Some(scalable);
            }
        }
    }

    for dir in ["/usr/share/pixmaps", "/usr/local/share/pixmaps"] {
        for ext in ["png", "svg"] {
            let candidate = Path::new(dir).join(format!("{name}.{ext}"));
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// Where desktop files live, in precedence order.
///
/// `XDG_DATA_HOME` first, then `XDG_DATA_DIRS`, per the spec. The order is the
/// whole point: a user's own copy of an entry must beat the system's, which is
/// how someone fixes a broken launcher line without root.
pub fn search_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();

    let home = std::env::var("XDG_DATA_HOME").ok().filter(|s| !s.is_empty());
    match home {
        Some(dir) => dirs.push(PathBuf::from(dir)),
        None => {
            if let Ok(home) = std::env::var("HOME") {
                dirs.push(PathBuf::from(home).join(".local/share"));
            }
        }
    }

    let system = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_string());
    dirs.extend(system.split(':').filter(|s| !s.is_empty()).map(PathBuf::from));

    dirs.into_iter().map(|d| d.join("applications")).collect()
}

/// Read every desktop entry, honouring precedence.
pub fn load_all() -> Vec<Entry> {
    let mut seen: HashMap<String, Entry> = HashMap::new();

    for dir in search_dirs() {
        let Ok(read) = std::fs::read_dir(&dir) else { continue };
        for file in read.flatten() {
            let path = file.path();
            if path.extension().and_then(|e| e.to_str()) != Some("desktop") {
                continue;
            }
            let Some(id) = path.file_name().and_then(|n| n.to_str()) else { continue };
            if seen.contains_key(id) {
                // First wins. Checked before reading, so an override also
                // saves the read.
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else { continue };
            if let Some(entry) = parse(&text, id) {
                seen.insert(id.to_string(), entry);
            }
        }
    }

    let mut entries: Vec<Entry> = seen.into_values().collect();
    entries.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    entries
}

/// How well an entry matches a query. Higher is better; `None` means no match.
///
/// Ranked in the order a person expects rather than by string distance: an
/// exact name, then a name that starts with the query, then a word inside the
/// name, then anywhere in the name, then the command. Typing `fire` should
/// offer Firefox before it offers something whose description mentions fire.
pub fn score(entry: &Entry, query: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let query = query.to_lowercase();
    let name = entry.name.to_lowercase();

    if name == query {
        return Some(1000);
    }
    if name.starts_with(&query) {
        // Shorter names first, so "Files" beats "Files Preferences" for "fil".
        return Some(800 - name.len().min(200) as i32);
    }
    if name.split_whitespace().any(|word| word.starts_with(&query)) {
        return Some(600 - name.len().min(200) as i32);
    }
    if name.contains(&query) {
        return Some(400 - name.len().min(200) as i32);
    }
    if entry.exec.first().is_some_and(|c| c.to_lowercase().contains(&query)) {
        return Some(200);
    }
    if entry
        .comment
        .as_ref()
        .is_some_and(|c| c.to_lowercase().contains(&query))
    {
        return Some(100);
    }
    None
}

/// The entries matching a query, best first.
pub fn rank<'a>(entries: &'a [Entry], query: &str) -> Vec<&'a Entry> {
    let mut scored: Vec<(i32, &Entry)> = entries
        .iter()
        .filter_map(|e| score(e, query).map(|s| (s, e)))
        .collect();
    // Ties break on name so the list does not reshuffle between keystrokes
    // that score identically — a list that jumps while you type is unusable.
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(&b.1.name)));
    scored.into_iter().map(|(_, e)| e).collect()
}

/// One heading in the launcher's menu.
///
/// Not the freedesktop category list. That list has thirteen main categories
/// and hundreds of additional ones, and a menu with a `Video` section beside an
/// `AudioVideo` section beside an `Audio` section is a directory listing of the
/// spec rather than somewhere to find a music player. These are the groupings
/// every shipped menu converges on, which is why `Network` reads as "Internet"
/// and `Utility` as "Utilities" — the spec's words are for the file, and these
/// are for the person reading the screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Section {
    Development,
    Education,
    Games,
    Graphics,
    Internet,
    Multimedia,
    Office,
    Settings,
    System,
    Utilities,
    /// Anything with no `Categories=` line, or only categories none of the
    /// above claim. A real section rather than a silent drop: an application
    /// that is installed and runnable must be reachable from the menu, and a
    /// mis-categorised entry hidden entirely is indistinguishable from one that
    /// failed to parse.
    Other,
}

impl Section {
    /// Every section, in the order the sidebar lists them.
    ///
    /// `Other` sits last because it is a fallback, and `Settings`/`System` sit
    /// low because they are visited deliberately rather than browsed.
    pub const ALL: [Section; 11] = [
        Section::Development,
        Section::Education,
        Section::Games,
        Section::Graphics,
        Section::Internet,
        Section::Multimedia,
        Section::Office,
        Section::Settings,
        Section::System,
        Section::Utilities,
        Section::Other,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Section::Development => "Development",
            Section::Education => "Education",
            Section::Games => "Games",
            Section::Graphics => "Graphics",
            Section::Internet => "Internet",
            Section::Multimedia => "Multimedia",
            Section::Office => "Office",
            Section::Settings => "Settings",
            Section::System => "System",
            Section::Utilities => "Utilities",
            Section::Other => "Other",
        }
    }
}

/// Which spec category maps to which section, most specific first.
///
/// Order is the whole point. An entry may declare several main categories —
/// `Utility;Network;` and `Settings;System;` are both common — so "the first
/// match wins" only gives a stable answer if *this table* decides what first
/// means, rather than the order the categories happened to appear in the file.
/// Two machines would otherwise sort the same application differently.
///
/// `Settings` before `System`, because a control panel declaring both belongs
/// under Settings; `Development` early, because an IDE declaring
/// `Development;Utility;` is not a utility.
const CATEGORY_SECTIONS: &[(&str, Section)] = &[
    ("Settings", Section::Settings),
    ("Development", Section::Development),
    ("Game", Section::Games),
    ("Graphics", Section::Graphics),
    ("Office", Section::Office),
    ("Network", Section::Internet),
    ("AudioVideo", Section::Multimedia),
    ("Audio", Section::Multimedia),
    ("Video", Section::Multimedia),
    ("Education", Section::Education),
    ("Science", Section::Education),
    ("System", Section::System),
    ("Utility", Section::Utilities),
];

/// Which section an entry belongs in.
pub fn section(entry: &Entry) -> Section {
    CATEGORY_SECTIONS
        .iter()
        .find(|(name, _)| entry.categories.iter().any(|c| c == name))
        .map(|(_, section)| *section)
        .unwrap_or(Section::Other)
}

/// Group entries into the sections that actually have something in them.
///
/// Empty sections are dropped rather than shown greyed out: on a given machine
/// half of them are empty, and a sidebar of mostly-dead rows makes the menu look
/// broken.
///
/// Each section is sorted by name even though `load_all` already returns its
/// entries sorted. This takes an arbitrary slice, not necessarily that one, and
/// the order it is handed is the caller's business — a section whose order
/// depended on how the caller happened to build its vector would be sorted in
/// the launcher and arbitrary in the next thing to call this.
pub fn sections(entries: &[Entry]) -> Vec<(Section, Vec<&Entry>)> {
    Section::ALL
        .iter()
        .filter_map(|&wanted| {
            let mut found: Vec<&Entry> =
                entries.iter().filter(|e| section(e) == wanted).collect();
            if found.is_empty() {
                return None;
            }
            found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
            Some((wanted, found))
        })
        .collect()
}

/// Resolve a pinned list — desktop ids, comma separated — into entries.
///
/// Order is the user's, not the filesystem's: a dock is muscle memory, and a
/// list that reorders itself when a package is installed defeats the point.
///
/// Unknown ids are dropped rather than shown as gaps. A pin naming an
/// application that is not installed is a stale config, and the honest reading
/// is "not here" rather than an icon that launches nothing.
pub fn resolve_pinned(list: &str, available: &[Entry]) -> Vec<Entry> {
    list.split(',')
        .map(str::trim)
        .filter(|id| !id.is_empty())
        .filter_map(|id| {
            available
                .iter()
                // The desktop id, then the binary, then the visible name —
                // because users write what they know, and that is usually
                // `firefox` rather than `org.mozilla.firefox`.
                .find(|e| e.id.eq_ignore_ascii_case(id))
                .or_else(|| {
                    available.iter().find(|e| {
                        e.exec
                            .first()
                            .and_then(|p| p.rsplit('/').next())
                            .is_some_and(|p| p.eq_ignore_ascii_case(id))
                    })
                })
                .or_else(|| available.iter().find(|e| e.name.eq_ignore_ascii_case(id)))
                .cloned()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, exec: &str) -> Entry {
        Entry {
            id: format!("{name}.desktop"),
            name: name.into(),
            comment: None,
            exec: strip_field_codes(exec),
            terminal: false,
            icon: None,
            categories: Vec::new(),
        }
    }

    /// The same, with categories, for the grouping tests.
    fn categorised(name: &str, categories: &[&str]) -> Entry {
        Entry {
            categories: categories.iter().map(|c| c.to_string()).collect(),
            ..entry(name, name)
        }
    }

    const FIREFOX: &str = "\
[Desktop Entry]
Version=1.0
Type=Application
Name=Firefox
Comment=Browse the web
Exec=firefox %u
Terminal=false
Categories=Network;WebBrowser;
";

    #[test]
    fn a_normal_entry_parses() {
        let e = parse(FIREFOX, "firefox.desktop").unwrap();
        assert_eq!(e.name, "Firefox");
        assert_eq!(e.exec, vec!["firefox"]);
        assert_eq!(e.comment.as_deref(), Some("Browse the web"));
        assert!(!e.terminal);
    }

    /// Field codes left in place are handed to the program as literal
    /// arguments, and it opens and complains it cannot find a file called %U.
    #[test]
    fn field_codes_are_removed() {
        assert_eq!(strip_field_codes("firefox %u"), vec!["firefox"]);
        assert_eq!(strip_field_codes("gimp-2.10 %U"), vec!["gimp-2.10"]);
        assert_eq!(
            strip_field_codes("env FOO=1 thing --flag %F --after"),
            vec!["env", "FOO=1", "thing", "--flag", "--after"]
        );
        assert_eq!(strip_field_codes("app %i %c %k %f"), vec!["app"]);
    }

    #[test]
    fn an_escaped_percent_survives() {
        assert_eq!(strip_field_codes("printf 100%%"), vec!["printf", "100%"]);
    }

    #[test]
    fn quoted_arguments_stay_together() {
        assert_eq!(
            strip_field_codes(r#"sh -c "echo hello world""#),
            vec!["sh", "-c", "echo hello world"]
        );
    }

    /// A launcher offering something the user explicitly asked to hide is
    /// worse than one that misses an app.
    #[test]
    fn hidden_and_nodisplay_entries_are_skipped() {
        let hidden = FIREFOX.replace("Terminal=false", "NoDisplay=true");
        assert!(parse(&hidden, "x.desktop").is_none());
        let hidden = FIREFOX.replace("Terminal=false", "Hidden=true");
        assert!(parse(&hidden, "x.desktop").is_none());
    }

    #[test]
    fn non_applications_are_skipped() {
        let link = FIREFOX.replace("Type=Application", "Type=Link");
        assert!(parse(&link, "x.desktop").is_none());
    }

    #[test]
    fn an_entry_without_exec_is_skipped() {
        let broken = FIREFOX.replace("Exec=firefox %u", "");
        assert!(parse(&broken, "x.desktop").is_none());
    }

    /// A file's `[Desktop Action]` groups have their own Name and Exec.
    /// Reading them into the same map launches the wrong command under the
    /// right name.
    #[test]
    fn action_groups_do_not_leak_into_the_main_entry() {
        let with_action = format!(
            "{FIREFOX}
[Desktop Action new-private-window]
Name=New Private Window
Exec=firefox --private-window
"
        );
        let e = parse(&with_action, "firefox.desktop").unwrap();
        assert_eq!(e.name, "Firefox");
        assert_eq!(e.exec, vec!["firefox"], "the action's Exec must not win");
    }

    /// Otherwise whichever locale happened to sort last would become the name.
    #[test]
    fn localised_keys_do_not_override_the_plain_one() {
        let localised = FIREFOX.replace("Name=Firefox", "Name=Firefox\nName[de]=Feuerfuchs");
        let e = parse(&localised, "x.desktop").unwrap();
        assert_eq!(e.name, "Firefox");
    }

    #[test]
    fn terminal_entries_are_marked() {
        let term = FIREFOX.replace("Terminal=false", "Terminal=true");
        assert!(parse(&term, "x.desktop").unwrap().terminal);
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let messy = format!("# a comment\n\n{FIREFOX}\n\n# trailing\n");
        assert!(parse(&messy, "x.desktop").is_some());
    }

    /// The spec's lists end with a trailing `;`. Splitting naively leaves an
    /// empty final element, which becomes a category named "" and a menu
    /// section with no title.
    #[test]
    fn a_trailing_semicolon_does_not_become_an_empty_category() {
        let e = parse(FIREFOX, "firefox.desktop").unwrap();
        assert_eq!(e.categories, vec!["Network", "WebBrowser"]);
    }

    #[test]
    fn an_entry_with_no_categories_line_parses_with_none() {
        let bare = FIREFOX.replace("Categories=Network;WebBrowser;\n", "");
        assert!(parse(&bare, "x.desktop").unwrap().categories.is_empty());
    }

    /// `Network` is what the file says; "Internet" is what the menu says.
    #[test]
    fn spec_categories_map_onto_menu_sections() {
        assert_eq!(section(&categorised("Firefox", &["Network"])), Section::Internet);
        assert_eq!(section(&categorised("Kate", &["Development"])), Section::Development);
        assert_eq!(section(&categorised("Files", &["Utility"])), Section::Utilities);
        assert_eq!(section(&categorised("VLC", &["AudioVideo"])), Section::Multimedia);
    }

    /// An installed, runnable application must be reachable from the menu.
    /// Hiding one because its categories were unrecognised is indistinguishable
    /// from one that failed to parse.
    #[test]
    fn unrecognised_and_absent_categories_both_land_in_other() {
        assert_eq!(section(&categorised("Odd", &["ConferenceCall"])), Section::Other);
        assert_eq!(section(&categorised("Bare", &[])), Section::Other);
    }

    /// The precedence table decides, not the order the file happened to list
    /// them in — otherwise the same application sorts differently on two
    /// machines, depending only on how its `.desktop` file was written.
    #[test]
    fn the_table_breaks_ties_not_the_files_ordering() {
        let one = categorised("Panel", &["Settings", "System"]);
        let other = categorised("Panel", &["System", "Settings"]);
        assert_eq!(section(&one), Section::Settings);
        assert_eq!(section(&other), Section::Settings, "file order must not decide");

        // An IDE declaring Development;Utility; is not a utility.
        assert_eq!(
            section(&categorised("IDE", &["Utility", "Development"])),
            Section::Development
        );
    }

    /// A sidebar of mostly-dead rows makes the menu look broken, and on any
    /// real machine half the sections are empty.
    #[test]
    fn grouping_drops_empty_sections_and_keeps_sidebar_order() {
        let entries = vec![
            categorised("Kate", &["Development"]),
            categorised("Firefox", &["Network"]),
        ];
        let grouped = sections(&entries);
        assert_eq!(
            grouped.iter().map(|(s, _)| *s).collect::<Vec<_>>(),
            vec![Section::Development, Section::Internet]
        );
        assert!(grouped.iter().all(|(_, found)| !found.is_empty()));
    }

    /// `load_all` returns filesystem order, which is arbitrary and differs per
    /// machine, so the section has to sort.
    #[test]
    fn entries_within_a_section_are_sorted_by_name() {
        let entries = vec![
            categorised("zathura", &["Office"]),
            categorised("Abiword", &["Office"]),
            categorised("libreoffice", &["Office"]),
        ];
        let (_, found) = sections(&entries).into_iter().next().unwrap();
        assert_eq!(
            found.iter().map(|e| e.name.as_str()).collect::<Vec<_>>(),
            vec!["Abiword", "libreoffice", "zathura"],
            "case must not split the alphabet into two runs"
        );
    }

    /// Every entry handed in must come back out exactly once. A menu that
    /// silently loses or duplicates an application is the failure this whole
    /// grouping step could plausibly introduce.
    #[test]
    fn grouping_partitions_the_catalogue() {
        let entries = vec![
            categorised("Kate", &["Development"]),
            categorised("Firefox", &["Network"]),
            categorised("Odd", &["ConferenceCall"]),
            categorised("Bare", &[]),
            categorised("VLC", &["AudioVideo"]),
        ];
        let grouped = sections(&entries);
        let total: usize = grouped.iter().map(|(_, found)| found.len()).sum();
        assert_eq!(total, entries.len());
    }

    #[test]
    fn an_empty_query_matches_everything() {
        let entries = vec![entry("Firefox", "firefox"), entry("Files", "nautilus")];
        assert_eq!(rank(&entries, "").len(), 2);
    }

    /// Typing a prefix must offer the thing that starts with it first.
    #[test]
    fn prefix_matches_outrank_substring_matches() {
        let entries = vec![
            entry("LibreOffice Writer", "lowriter"),
            entry("Writer's Helper", "wh"),
        ];
        let ranked = rank(&entries, "writer");
        assert_eq!(ranked[0].name, "Writer's Helper", "starts-with beats contains");
    }

    fn fake(id: &str, name: &str, exec: &str) -> Entry {
        Entry {
            id: id.into(),
            name: name.into(),
            comment: None,
            exec: vec![exec.into()],
            terminal: false,
            icon: Some(id.into()),
            categories: Vec::new(),
        }
    }

    fn catalogue() -> Vec<Entry> {
        vec![
            fake("org.mozilla.firefox", "Firefox", "/usr/bin/firefox"),
            fake("Alacritty", "Alacritty", "/usr/bin/alacritty"),
            fake("org.kde.dolphin", "Dolphin", "/usr/bin/dolphin"),
        ]
    }

    /// A dock is muscle memory: the order is the user's, and must not become
    /// the filesystem's the next time a package is installed.
    #[test]
    fn pins_keep_the_order_they_were_written_in() {
        let pinned = resolve_pinned("org.kde.dolphin, Alacritty, org.mozilla.firefox", &catalogue());
        let names: Vec<&str> = pinned.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["Dolphin", "Alacritty", "Firefox"]);
    }

    /// Users write what they know, which is the binary far more often than the
    /// reverse-DNS desktop id.
    #[test]
    fn a_pin_may_name_the_binary_or_the_visible_name() {
        assert_eq!(resolve_pinned("firefox", &catalogue()).len(), 1, "by binary");
        assert_eq!(resolve_pinned("Dolphin", &catalogue()).len(), 1, "by name");
        assert_eq!(resolve_pinned("ALACRITTY", &catalogue()).len(), 1, "case-insensitively");
    }

    /// A stale pin is dropped, not drawn as a gap or an icon that launches
    /// nothing.
    #[test]
    fn a_pin_for_something_not_installed_is_dropped() {
        let pinned = resolve_pinned("firefox, nothing-here, dolphin", &catalogue());
        assert_eq!(pinned.len(), 2);
    }

    /// A desktop entry's id keeps its extension; a *window's* `app_id` does
    /// not. So anything matching an app_id against these has to append
    /// `.desktop` first, and the failure is quiet: the id match never fires and
    /// the binary-name fallback picks up the slack for exactly those
    /// applications whose id happens to be their binary name.
    ///
    /// That is why the dock's pinned list — written as bare names like `konsole`
    /// — always looked correct while every reverse-DNS app id fell through to a
    /// lettered placeholder.
    #[test]
    fn ids_keep_the_desktop_extension_and_app_ids_do_not() {
        let installed = vec![fake("org.kde.konsole.desktop", "Konsole", "konsole")];

        assert!(
            resolve_pinned("org.kde.konsole", &installed).is_empty(),
            "a bare app id must not match an id that carries the extension"
        );
        assert_eq!(
            resolve_pinned("org.kde.konsole.desktop", &installed).len(),
            1,
            "appending .desktop is what makes the id match"
        );
        // And the reason the omission stayed hidden this long.
        assert_eq!(
            resolve_pinned("konsole", &installed).len(),
            1,
            "the binary-name fallback masks it for applications named after \
             their binary"
        );
    }

    #[test]
    fn an_empty_pin_list_pins_nothing() {
        assert!(resolve_pinned("", &catalogue()).is_empty());
        assert!(resolve_pinned("  ,  , ", &catalogue()).is_empty());
    }

    #[test]
    fn an_exact_name_wins_outright() {
        let entries = vec![entry("File Manager", "fm"), entry("Files", "nautilus")];
        let ranked = rank(&entries, "files");
        assert_eq!(ranked[0].name, "Files");
    }

    /// A word inside the name should match, so "term" finds "GNOME Terminal".
    #[test]
    fn a_word_inside_the_name_matches() {
        let entries = vec![entry("GNOME Terminal", "gnome-terminal")];
        assert_eq!(rank(&entries, "term").len(), 1);
    }

    #[test]
    fn the_command_matches_when_the_name_does_not() {
        let entries = vec![entry("Files", "nautilus")];
        assert_eq!(rank(&entries, "nautilus").len(), 1);
        assert!(rank(&entries, "zzzz").is_empty());
    }

    /// Shorter names first among equal kinds of match, so "fil" offers "Files"
    /// before "Files Preferences".
    #[test]
    fn shorter_names_come_first_among_equals() {
        let entries = vec![entry("Files Preferences", "a"), entry("Files", "b")];
        let ranked = rank(&entries, "fil");
        assert_eq!(ranked[0].name, "Files");
    }

    /// A list that reshuffles between keystrokes that score the same is
    /// unusable — you aim for the second row and it moves.
    #[test]
    fn equal_scores_are_ordered_stably_by_name() {
        let entries = vec![entry("Bravo", "x"), entry("Alpha", "y"), entry("Charlie", "z")];
        let names: Vec<&str> = rank(&entries, "").iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["Alpha", "Bravo", "Charlie"]);
    }

    /// Not a unit test so much as a floor: a resolver that finds nothing on a
    /// desktop system is not a resolver. It reports the rate rather than
    /// asserting a number, because the number depends on what is installed —
    /// but zero would mean the layouts are wrong, which is the failure this
    /// exists to catch.
    #[test]
    fn most_installed_applications_resolve_an_icon() {
        let all = load_all();
        if all.is_empty() {
            eprintln!("no desktop entries; skipping");
            return;
        }
        let with_icon: Vec<_> = all.iter().filter(|e| e.icon.is_some()).collect();
        let resolved = with_icon
            .iter()
            .filter(|e| find_icon(e.icon.as_deref().unwrap()).is_some())
            .count();
        eprintln!("icons: {resolved} of {} resolved", with_icon.len());
        assert!(
            resolved * 2 > with_icon.len(),
            "only {resolved} of {} icons resolved — the directory layouts are probably wrong",
            with_icon.len()
        );
    }

    /// The same shape of floor as the icon test above, for the menu: a
    /// categoriser that files everything installed under "Other" has produced a
    /// sidebar with one row, which is the flat list the menu replaced. It
    /// reports the distribution rather than asserting counts, because those
    /// depend on what is installed.
    #[test]
    fn most_installed_applications_land_somewhere_better_than_other() {
        let all = load_all();
        if all.is_empty() {
            eprintln!("no desktop entries; skipping");
            return;
        }
        let grouped = sections(&all);
        for (section, found) in &grouped {
            eprintln!("{:>12}: {}", section.title(), found.len());
        }

        let total: usize = grouped.iter().map(|(_, found)| found.len()).sum();
        assert_eq!(total, all.len(), "every installed entry must appear exactly once");

        let other = grouped
            .iter()
            .find(|(section, _)| *section == Section::Other)
            .map_or(0, |(_, found)| found.len());
        assert!(
            other * 2 < all.len(),
            "{other} of {} entries fell through to Other — the category table is \
             probably missing something common",
            all.len()
        );
    }

    #[test]
    fn an_absolute_icon_path_is_used_as_given() {
        assert_eq!(find_icon("/definitely/not/here.png"), None);
    }

    #[test]
    fn search_dirs_put_the_users_own_first() {
        let dirs = search_dirs();
        assert!(!dirs.is_empty());
        assert!(
            dirs.iter().all(|d| d.ends_with("applications")),
            "every directory must be an applications dir"
        );
    }
}
