//! The battery and network readouts, polled off the UI thread.
//!
//! Same arrangement as the tray: a thread writes, the view reads, and nothing
//! the strip draws can be blocked by something the bus is slow about. It
//! matters more here than it looks, because the failure is asymmetric — a slow
//! `battery` read costs a frame, and a D-Bus call to a service that is starting
//! up can block for its activation timeout, which is tens of seconds. On the UI
//! thread that is a frozen dock.
//!
//! # Why this polls at all
//!
//! Both sources can push. UPower and NetworkManager emit `PropertiesChanged`,
//! and a signal-driven version would use less power than waking twice a second
//! forever.
//!
//! It polls anyway, for now, because half of this has no daemon to push from:
//! `battery` reads sysfs directly and sysfs has no notification. Making the
//! network half event-driven and leaving the battery half on a timer would mean
//! the thread still wakes on the timer, and would buy nothing but two code
//! paths. If this ever moves to UPower, both halves become signals together —
//! and that is the point at which the change is worth making.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::battery::Battery;
use crate::network::{Network, Watcher};

/// How often the readouts are refreshed.
///
/// Two seconds, chosen from the faster of the two. A battery percentage moves
/// once every several minutes and would be happy with thirty; unplugging an
/// ethernet cable should change the icon before you have finished looking at
/// it. The cost is a handful of small file reads and about five D-Bus property
/// gets on a local socket, which is not enough to be worth two timers.
const EVERY: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Status {
    /// `None` on a machine with no battery, which is a normal state and not a
    /// failure — the strip draws nothing rather than a zero.
    pub battery: Option<Battery>,
    pub network: Network,
}

impl Default for Status {
    fn default() -> Self {
        Status {
            battery: None,
            network: Network::offline(),
        }
    }
}

pub type Shared = Arc<Mutex<Status>>;

/// Start polling. The returned handle is read by the view each tick.
pub fn start() -> Shared {
    let shared: Shared = Arc::new(Mutex::new(Status::default()));
    let published = shared.clone();

    std::thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
            Ok(runtime) => runtime,
            Err(e) => {
                // The battery half needs no runtime and could still work. It
                // is given up anyway: a strip showing charge and permanently
                // claiming no network is worse than one showing neither,
                // because only the second is obviously broken.
                eprintln!("status: no runtime, battery and network readouts disabled: {e}");
                return;
            }
        };

        runtime.block_on(async move {
            let mut watcher = Watcher::default();
            loop {
                let fresh = Status {
                    battery: crate::battery::read(),
                    network: watcher.read().await,
                };
                if let Ok(mut held) = published.lock() {
                    // Compared before assigning, for the reason the tray's
                    // poll is: iced rebuilds its view whenever state changes,
                    // and writing an identical reading twice a second would
                    // redraw the dock forever for nothing.
                    if *held != fresh {
                        *held = fresh;
                    }
                }
                tokio::time::sleep(EVERY).await;
            }
        });
    });

    shared
}
