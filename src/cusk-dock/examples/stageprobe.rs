//! Minimise a window and check a picture of it comes back.
//!
//! `cargo run --example stageprobe` against a running cusk with at least one
//! window open. Same reason as `minprobe`: the dock cannot run headless, so
//! the only other way to test the stage is to minimise something and look at
//! the strip — which cannot tell a protocol that sent nothing from a protocol
//! that sent a picture the dock failed to draw.
//!
//! Checks the things that are actually easy to get wrong, in order: that the
//! global exists at all, that an event arrives, that the picture has plausible
//! geometry, that it is not blank, and that restoring clears it. A stage that
//! never clears is the failure that looks fine until you restore a window and
//! its ghost stays on the strip.

use std::time::Duration;

use cusk_dock::windows::{self, Request, Window};

/// Long enough for a minimise to cross to the compositor, be acted on, be
/// captured on the next frame, and come back as an `image` event. Longer than
/// `minprobe`'s because the capture is deliberately deferred a frame.
const SETTLE: Duration = Duration::from_millis(1200);

fn main() {
    let (shared, outbox, thumbs) = windows::start();
    std::thread::sleep(Duration::from_secs(1));

    let Some(target) = pick(&shared) else {
        eprintln!("stageprobe: no window to test with — open one and try again");
        std::process::exit(2);
    };
    println!("target: id={} {:?}", target.id, target.label());

    if state_of(&shared, target.id).is_none_or(|w| w.minimized) {
        eprintln!("stageprobe: target is already minimised; cannot test the transition");
        std::process::exit(2);
    }

    // A thumbnail before anything has been minimised would make every later
    // assertion pass for the wrong reason.
    if thumbs.lock().is_ok_and(|held| held.contains_key(&target.id)) {
        eprintln!("stageprobe: the compositor already has a thumbnail for a visible window");
        std::process::exit(2);
    }

    let mut failures = 0;

    outbox.push(target.id, Request::Minimize);
    std::thread::sleep(SETTLE);

    match thumbs.lock().ok().and_then(|held| held.get(&target.id).cloned()) {
        None => {
            println!("  FAIL  minimised, but no thumbnail arrived");
            println!("        — the compositor may not offer hadal_stage_manager_v1,");
            println!("          or `watch` never reached it, or capture failed silently");
            failures += 1;
        }
        Some(thumbnail) => {
            println!(
                "  PASS  thumbnail arrived: {}x{}, {} bytes",
                thumbnail.width,
                thumbnail.height,
                thumbnail.pixels.len()
            );

            // The length and the geometry have to agree, or the dock is one
            // `from_rgba` away from drawing whatever is next in memory.
            let want = thumbnail.width as usize * thumbnail.height as usize * 4;
            if thumbnail.pixels.len() == want {
                println!("  PASS  length agrees with the dimensions");
            } else {
                println!("  FAIL  {} bytes for a {want}-byte image", thumbnail.pixels.len());
                failures += 1;
            }

            // Bounded on the long edge, and not by a lot. A full-size capture
            // arriving here would mean `downscale` was skipped, which is a
            // working picture and a quarter of a megabyte per window turning
            // into several.
            let long = thumbnail.width.max(thumbnail.height);
            if long <= 256 {
                println!("  PASS  scaled down: long edge {long}px");
            } else {
                println!("  FAIL  long edge {long}px — this is not a thumbnail");
                failures += 1;
            }

            // The failure that looks like success. A right-sized buffer of
            // zeroes is what a capture that rendered nothing produces, and on
            // the strip it is an empty tile — indistinguishable from a dock
            // that failed to draw.
            if thumbnail.pixels.iter().any(|&b| b != 0) {
                println!("  PASS  and there is something in it");
            } else {
                println!("  FAIL  the thumbnail is entirely zero — nothing was rendered");
                failures += 1;
            }

            // Premultiplied alpha survives `unpack` as a specific artefact:
            // every colour channel clamped at or below its alpha. Not a proof,
            // but it catches the conversion being dropped entirely.
            let premultiplied = thumbnail
                .pixels
                .chunks_exact(4)
                .filter(|px| px[3] != 0 && px[3] != 255)
                .take(64)
                .collect::<Vec<_>>();
            if !premultiplied.is_empty() {
                let capped = premultiplied
                    .iter()
                    .all(|px| px[0] <= px[3] && px[1] <= px[3] && px[2] <= px[3]);
                if capped {
                    println!("  WARN  every translucent pixel is darker than its alpha —");
                    println!("        the premultiplied-to-straight conversion may not have run");
                } else {
                    println!("  PASS  translucent pixels look like straight alpha");
                }
            }
        }
    }

    outbox.push(target.id, Request::Unminimize);
    std::thread::sleep(SETTLE);

    if thumbs.lock().is_ok_and(|held| held.contains_key(&target.id)) {
        println!("  FAIL  the thumbnail outlived the restore — the strip would keep a ghost");
        failures += 1;
    } else {
        println!("  PASS  cleared on restore");
    }

    // Restoring must not have cost the window itself, which is what a `clear`
    // wired to the wrong id would look like from here.
    match state_of(&shared, target.id) {
        Some(w) if !w.minimized => println!("  PASS  and the window came back"),
        Some(_) => {
            println!("  FAIL  still minimised");
            failures += 1;
        }
        None => {
            println!("  FAIL  the window disappeared");
            failures += 1;
        }
    }

    if failures == 0 {
        println!("\nthe stage works end to end");
    } else {
        println!("\n{failures} failure(s)");
        std::process::exit(1);
    }
}

/// A window worth testing with. Same rule as `minprobe`: not one of the dock's
/// own surfaces, since minimising the launcher mid-test is confusing rather
/// than informative.
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
