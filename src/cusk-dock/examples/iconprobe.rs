//! Does an app_id resolve to an icon? Checks the exact path the left dock uses.
//!
//! The dock cannot be inspected on screen without a screenshot tool, so this
//! runs `resolve_pinned` + `find_icon` over app_ids taken off the wire and says
//! what the strip would draw: an icon, or a lettered fallback.
fn main() {
    let installed = cusk::entry::load_all();
    for app_id in std::env::args().skip(1) {
        // The same two-step the dock uses: suffixed id first, bare name second.
        let matched = {
            let suffixed =
                cusk::entry::resolve_pinned(&format!("{app_id}.desktop"), &installed);
            if suffixed.is_empty() {
                cusk::entry::resolve_pinned(&app_id, &installed)
            } else {
                suffixed
            }
        };
        match matched.first() {
            None => println!("{app_id}: NO desktop entry matched -> letter tile"),
            Some(entry) => {
                let icon = entry.icon.as_deref().and_then(cusk::entry::find_icon);
                match icon {
                    Some(path) => println!("{app_id}: {} -> {}", entry.name, path.display()),
                    None => println!("{app_id}: matched {} but no icon file -> letter tile", entry.name),
                }
            }
        }
    }
}
