//! Drive the outbox end to end: list windows, minimise one, read it back.
//!
//! The dock's tiles cannot be clicked from a script, so this exercises the same
//! path a click takes — queue a Request, let the event thread send it, and see
//! whether the compositor's reply changes the snapshot. Without this the whole
//! request half of the protocol is untested against a real compositor.
fn main() {
    let (windows, outbox, _thumbs) = cusk_dock::windows::start();
    std::thread::sleep(std::time::Duration::from_secs(2));

    // The *unfocused* window, so a successful Activate is visible as a change.
    let target = windows.lock().unwrap().iter().find(|w| !w.activated).cloned();
    let Some(target) = target else {
        println!("no windows to act on");
        return;
    };
    println!("before: id={} activated={}", target.id, target.activated);

    outbox.push(target.id, cusk_dock::windows::Request::Activate);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let after = windows
        .lock()
        .unwrap()
        .iter()
        .find(|w| w.id == target.id)
        .cloned();
    match after {
        Some(w) => println!("after Activate: activated={}", w.activated),
        None => println!("after Activate: window vanished"),
    }

    // And what set_minimized does, which is the thing Stage Manager will need.
    outbox.push(target.id, cusk_dock::windows::Request::Minimize);
    std::thread::sleep(std::time::Duration::from_secs(2));
    let after = windows
        .lock()
        .unwrap()
        .iter()
        .find(|w| w.id == target.id)
        .cloned();
    if let Some(w) = after {
        println!("after Minimize: minimized={} (cusk ignores it today)", w.minimized);
    }
}
