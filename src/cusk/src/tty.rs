//! The tty backend, phase one: finding out what is actually there.
//!
//! Running cusk on a virtual terminal is the thing that turns it from a window
//! inside someone else's session into a session of its own. It is also the
//! largest single piece of work in the project — session management, DRM
//! mode-setting, GBM buffers, libinput and udev hotplug — and the failure mode
//! is unusually harsh: a compositor that takes DRM master and then gets the
//! mode wrong leaves a black screen on a VT with no way back but a hard reset.
//!
//! So it is being built in phases, and this is the one that cannot hurt
//! anything.
//!
//! # What this does
//!
//! Opens a real session through libseat, enumerates the DRM devices udev
//! reports, and prints every connector, its status, and the modes it offers.
//! Then exits.
//!
//! # What this deliberately does not do
//!
//! **It never becomes DRM master.** Master is exclusive, and on this machine
//! KWin already holds it; asking for it would either fail or take the display
//! away from the running session. Reading resources — connectors, encoders,
//! modes — needs no master, which is exactly why this much can be checked from
//! inside a working desktop.
//!
//! What it proves: the feature flags compile against the system libraries, and
//! the device can be opened and interrogated. Those are expensive to discover
//! *after* writing the mode-setting code rather than before.
//!
//! # Session control is exclusive, and that is the finding
//!
//! Acquiring a session is attempted and is **expected to fail from inside a
//! running desktop**. logind grants `TakeControl` to one process per session,
//! and the compositor already running holds it; libseat then finds no usable
//! backend and reports `ENOSYS`. That is not a misconfiguration, it is the
//! design — two session controllers on one session would each think they owned
//! the input devices.
//!
//! So the probe falls back to opening the device directly, which needs only
//! membership of the `video` group and tells us everything phase one wants to
//! know. The real backend will have to run from its own VT, and finding that
//! out here costs nothing.

use std::path::PathBuf;

use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::Session;
use smithay::reexports::drm::control::{connector, Device as ControlDevice};
use smithay::reexports::rustix;

/// A DRM device as found, with whatever could be read from it.
#[derive(Debug)]
pub struct Card {
    pub path: PathBuf,
    pub connectors: Vec<Connector>,
    /// Why the device could not be interrogated, if it could not be.
    pub error: Option<String>,
}

#[derive(Debug)]
pub struct Connector {
    pub name: String,
    pub connected: bool,
    /// Width, height and refresh of each advertised mode.
    pub modes: Vec<(u16, u16, u32)>,
}

/// A DRM device opened through the session, closed again on drop.
struct Opened {
    file: std::fs::File,
}

impl std::os::fd::AsFd for Opened {
    fn as_fd(&self) -> std::os::fd::BorrowedFd<'_> {
        std::os::fd::AsFd::as_fd(&self.file)
    }
}
impl smithay::reexports::drm::Device for Opened {}
impl ControlDevice for Opened {}

/// How the devices were reached.
pub enum Access {
    /// Through libseat, which is how the real backend will do it.
    Session(String),
    /// Directly, because no session could be taken. Carries the reason.
    Direct(String),
}

/// Enumerate DRM devices and what they are connected to.
pub fn probe() -> Result<(Access, Vec<Card>), String> {
    let paths = drm_devices();
    if paths.is_empty() {
        return Err("no DRM devices found under /dev/dri".into());
    }

    // The way the real backend will do it: logind hands out the device, so a
    // compositor never needs root. Expected to fail here — see the module
    // note — and the fallback is what makes the probe useful anyway.
    match LibSeatSession::new() {
        Ok((mut session, _notifier)) => {
            let seat = session.seat();
            let cards = paths
                .into_iter()
                .map(|path| {
                    match session.open(
                        &path,
                        rustix::fs::OFlags::RDWR
                            | rustix::fs::OFlags::CLOEXEC
                            | rustix::fs::OFlags::NONBLOCK,
                    ) {
                        Ok(fd) => interrogate(&Opened { file: std::fs::File::from(fd) }, path),
                        Err(e) => Card {
                            path,
                            connectors: Vec::new(),
                            error: Some(format!("session refused the device: {e}")),
                        },
                    }
                })
                .collect();
            Ok((Access::Session(seat), cards))
        }
        Err(e) => {
            let cards = paths
                .into_iter()
                .map(|path| match std::fs::File::open(&path) {
                    Ok(file) => interrogate(&Opened { file }, path),
                    Err(e) => Card {
                        path,
                        connectors: Vec::new(),
                        error: Some(format!("could not open: {e}")),
                    },
                })
                .collect();
            Ok((Access::Direct(e.to_string()), cards))
        }
    }
}

/// Read a device's connectors and their modes.
///
/// Every failure is recorded rather than propagated: one unreadable device
/// among several is worth reporting *and* continuing past, and a probe that
/// stops at the first problem hides the state of everything after it.
fn interrogate(device: &Opened, path: PathBuf) -> Card {
    let resources = match device.resource_handles() {
        Ok(resources) => resources,
        Err(e) => {
            return Card {
                path,
                connectors: Vec::new(),
                error: Some(format!("could not read resources: {e}")),
            }
        }
    };

    let mut connectors = Vec::new();
    for handle in resources.connectors() {
        let Ok(info) = device.get_connector(*handle, false) else { continue };
        connectors.push(Connector {
            name: format!("{:?}-{}", info.interface(), info.interface_id()),
            connected: info.state() == connector::State::Connected,
            modes: info
                .modes()
                .iter()
                .map(|mode| {
                    let (w, h) = mode.size();
                    (w, h, mode.vrefresh())
                })
                .collect(),
        });
    }

    Card { path, connectors, error: None }
}

/// The DRM devices present, as paths.
///
/// Read straight from `/dev/dri` rather than through udev. Phase one only
/// needs to know what exists; udev's value is hotplug and seat assignment,
/// which belong with the backend that has to react to them.
fn drm_devices() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir("/dev/dri") else { return Vec::new() };
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                // Only mode-setting nodes. `renderD*` can render but cannot
                // drive a display, so a compositor that picked one would come
                // up with nowhere to put the picture.
                .is_some_and(|name| name.starts_with("card"))
        })
        .collect();
    paths.sort();
    paths
}

/// Print what was found, in a form that answers "will this work".
pub fn report(access: &Access, cards: &[Card]) {
    println!();
    match access {
        Access::Session(seat) => println!("  session acquired, seat: {seat}"),
        Access::Direct(reason) => {
            println!("  no session ({reason})");
            println!("  reading devices directly instead — this is expected inside a");
            println!("  running desktop, because session control is held by one process");
            println!("  at a time and the compositor already running holds it.");
        }
    }
    for card in cards {
        println!();
        println!("  {}", card.path.display());
        if let Some(error) = &card.error {
            println!("      {error}");
            continue;
        }
        if card.connectors.is_empty() {
            println!("      no connectors");
        }
        for connector in &card.connectors {
            let state = if connector.connected { "connected" } else { "disconnected" };
            println!("      {:<12} {}", connector.name, state);
            if let Some((w, h, refresh)) = connector.modes.first() {
                println!("          preferred  {w}x{h}@{refresh}");
            }
            if connector.modes.len() > 1 {
                println!("          {} modes total", connector.modes.len());
            }
        }
    }
    println!();

    let usable = cards
        .iter()
        .any(|card| card.error.is_none() && card.connectors.iter().any(|c| c.connected));
    if usable {
        println!("  A display is reachable and its modes are readable.");
    } else {
        println!("  No connected display was readable — see the errors above.");
    }
    if matches!(access, Access::Direct(_)) {
        println!();
        println!("  To test the session path, run this from a free VT");
        println!("  (ctrl+alt+F3, log in, then run cusk --probe-drm).");
    }
    println!();
}
