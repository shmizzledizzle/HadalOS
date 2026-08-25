//! Minimise a window and restore it, over the real protocol, with no mouse.
//!
//! `cargo run --example minprobe` against a running cusk that has at least one
//! window open. Exists for the same reason `actprobe.rs` does: the dock cannot
//! run headless, so the only other way to test minimising is to right-click a
//! tile and watch — which is not a test, and which was how `set_minimized`
//! managed to be a no-op in the compositor for as long as it was.
//!
//! Prints a verdict rather than a dump. The interesting states are all
//! transitions, and a wall of window lists makes the reader do the diffing.

use std::time::Duration;

use cusk_dock::windows::{self, Request, Window};

/// Long enough for a request to cross to the compositor, be acted on, and come
/// back as a state change. The outbox wakes the event thread immediately, so
/// this is a round trip and not a poll interval.
const SETTLE: Duration = Duration::from_millis(600);

fn main() {
    let (shared, outbox, _thumbs) = windows::start();
    std::thread::sleep(Duration::from_secs(1));

    let Some(target) = pick(&shared) else {
        eprintln!("minprobe: no window to test with — open one and try again");
        std::process::exit(2);
    };
    println!("target: id={} {:?}", target.id, target.label());

    let mut failures = 0;

    // Precondition, checked rather than assumed: a window that is already
    // minimised would make the first assertion pass for the wrong reason.
    if state_of(&shared, target.id).is_none_or(|w| w.minimized) {
        eprintln!("minprobe: target is already minimised; cannot test the transition");
        std::process::exit(2);
    }

    outbox.push(target.id, Request::Minimize);
    std::thread::sleep(SETTLE);
    match state_of(&shared, target.id) {
        Some(w) if w.minimized => println!("  PASS  minimised, and the compositor said so"),
        Some(_) => {
            println!("  FAIL  still not minimised — set_minimized reached nothing");
            failures += 1;
        }
        // A minimised window must stay in the list. Vanishing from it would
        // mean no taskbar could offer a way back, which is the whole reason
        // minimising was refused before it was implemented.
        None => {
            println!("  FAIL  the window left the list entirely; nothing could restore it");
            failures += 1;
        }
    }

    outbox.push(target.id, Request::Unminimize);
    std::thread::sleep(SETTLE);
    match state_of(&shared, target.id) {
        Some(w) if !w.minimized => println!("  PASS  restored"),
        Some(_) => {
            println!("  FAIL  still minimised after unminimise");
            failures += 1;
        }
        None => {
            println!("  FAIL  the window disappeared during restore");
            failures += 1;
        }
    }

    // Restoring should also focus: `unminimize` calls `focus`, and a window
    // that comes back unfocused means a second click to type into the thing
    // just chosen.
    match state_of(&shared, target.id) {
        Some(w) if w.activated => println!("  PASS  and it came back focused"),
        Some(_) => println!("  WARN  restored but not activated"),
        None => {}
    }

    if failures == 0 {
        println!("\nminimise round trip works");
    } else {
        println!("\n{failures} failure(s)");
        std::process::exit(1);
    }
}

/// A window worth testing with.
///
/// Skips the dock's own strips: they are layer surfaces and never appear here,
/// but the launcher is an ordinary client and minimising it mid-test would be
/// confusing rather than informative.
fn pick(shared: &windows::Shared) -> Option<Window> {
    let held = shared.lock().ok()?;
    held.iter()
        .find(|w| !w.app_id.starts_with("cusk-"))
        .or_else(|| held.first())
        .cloned()
}

fn state_of(shared: &windows::Shared, id: u32) -> Option<Window> {
    let held = shared.lock().ok()?;
    held.iter().find(|w| w.id == id).cloned()
}
