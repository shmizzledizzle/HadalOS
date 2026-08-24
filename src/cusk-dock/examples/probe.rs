//! Print the window list once and exit, for checking the protocol by hand.
//!
//! `cargo run --example probe` against a running cusk. Exists because the dock
//! cannot be run headless — it needs a compositor to draw into — so a bug in
//! the window list would otherwise only be visible as an empty strip.
fn main() {
    let (windows, _outbox) = cusk_dock::windows::start();
    std::thread::sleep(std::time::Duration::from_secs(2));
    let held = windows.lock().unwrap();
    println!("{} window(s)", held.len());
    for w in held.iter() {
        println!(
            "  id={} activated={} minimized={} app_id={:?} title={:?}",
            w.id, w.activated, w.minimized, w.app_id, w.title
        );
    }
}
