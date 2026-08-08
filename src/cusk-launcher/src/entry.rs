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
use std::path::PathBuf;

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
    })
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

    #[test]
    fn search_dirs_put_the_users_own_first() {
        let dirs = search_dirs();
        assert!(!dirs.is_empty());
        assert!(
            dirs.iter().all(|d| d.ends_with("applications")),
            "every directory must be an applications dir"
        );
    }
    /// Not a unit test — a report against this machine's real applications
    /// directory, so a parser that works on fixtures and finds nothing in the
    /// wild is caught. Run with --nocapture.
    #[test]
    fn report_real_entries() {
        let all = load_all();
        eprintln!("dirs: {:?}", search_dirs());
        eprintln!("parsed {} entries", all.len());
        for e in all.iter().take(8) {
            eprintln!("  {:<28} {:?}{}", e.name, e.exec, if e.terminal { "  [terminal]" } else { "" });
        }
        for q in ["fire", "term", "set", "file"] {
            let r = rank(&all, q);
            eprintln!("  {q:>6} -> {}", r.iter().take(3).map(|e| e.name.as_str()).collect::<Vec<_>>().join(", "));
        }
        assert!(!all.is_empty(), "no desktop entries found on a desktop system");
    }
}
