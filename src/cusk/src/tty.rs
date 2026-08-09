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
use std::sync::Arc;

use smithay::backend::libinput::LibinputSessionInterface;
use smithay::backend::session::libseat::LibSeatSession;
use smithay::backend::session::Session;
use smithay::reexports::drm::buffer::DrmFourcc;
use smithay::reexports::drm::control::{connector, crtc, framebuffer, Device as ControlDevice};
use smithay::reexports::drm::{self, Device as DrmDevice};
use smithay::reexports::input;
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
impl DrmDevice for Opened {}
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

// ── phase two: setting a mode ────────────────────────────────────────────

/// What the CRTC was doing before we touched it.
///
/// Saved so it can be put back. Restoring is not a courtesy — a compositor
/// that takes DRM master, sets a mode and exits leaves the display showing
/// whatever was last scanned out, with no text console to type into.
#[derive(Debug, Clone, Copy)]
struct Previous {
    crtc: crtc::Handle,
    mode: Option<drm::control::Mode>,
    framebuffer: Option<framebuffer::Handle>,
    position: (u32, u32),
    connector: connector::Handle,
}

impl Previous {
    fn restore(&self, card: &Opened) {
        // Errors are reported and not propagated. This runs on the way out,
        // including from the watchdog, and there is nothing above it that
        // could do anything more useful than say so.
        //
        // Except a revoked device, which is not a failure: it means another VT
        // owns the display, so there is no mode of ours left to put back, and
        // saying "could not restore" there is alarming and wrong.
        if let Err(e) = card.set_crtc(
            self.crtc,
            self.framebuffer,
            self.position,
            &[self.connector],
            self.mode,
        ) {
            if Drm::is_revoked(&e) {
                tracing::debug!("no mode to restore; another VT owns the display");
            } else {
                eprintln!("  could not restore the previous mode: {e}");
            }
        }
    }
}

/// Set a mode on the first connected output, show a colour, and put it back.
///
/// `seconds` is how long the colour stays up. The whole point of this phase is
/// that it cannot outstay it: a watchdog thread restores the mode and kills the
/// process regardless of what the main thread is doing, and it is armed
/// **before** master is taken so a hang inside mode-setting cannot escape it.
pub fn modeset(seconds: u64) -> Result<(), String> {
    let (mut session, _notifier) =
        LibSeatSession::new().map_err(|e| format!("could not join a session: {e}\n  \
             This needs its own VT — see --probe-drm."))?;
    let seat = session.seat();

    let path = drm_devices()
        .into_iter()
        .next()
        .ok_or("no DRM device under /dev/dri")?;
    let fd = session
        .open(
            &path,
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
        )
        .map_err(|e| format!("session refused {}: {e}", path.display()))?;
    let card = Arc::new(Opened { file: std::fs::File::from(fd) });

    // Pick the target before touching anything, so a machine with nothing
    // connected fails having changed no state at all.
    let resources = card
        .resource_handles()
        .map_err(|e| format!("could not read resources: {e}"))?;
    let (connector_handle, mode) = resources
        .connectors()
        .iter()
        .filter_map(|handle| card.get_connector(*handle, true).ok())
        .filter(|info| info.state() == connector::State::Connected)
        .find_map(|info| info.modes().first().copied().map(|mode| (info.handle(), mode)))
        .ok_or("no connected output with a mode")?;

    let connector_info = card
        .get_connector(connector_handle, false)
        .map_err(|e| format!("could not re-read the connector: {e}"))?;
    let name = format!("{:?}-{}", connector_info.interface(), connector_info.interface_id());

    // The CRTC currently driving this connector, via its encoder. Reusing the
    // existing one rather than picking any free CRTC is what makes the saved
    // state and the restore refer to the same hardware.
    let crtc_handle = connector_info
        .current_encoder()
        .and_then(|handle| card.get_encoder(handle).ok())
        .and_then(|encoder| encoder.crtc())
        .or_else(|| resources.crtcs().first().copied())
        .ok_or("no CRTC available for that connector")?;

    let info = card
        .get_crtc(crtc_handle)
        .map_err(|e| format!("could not read the CRTC: {e}"))?;
    let previous = Previous {
        crtc: crtc_handle,
        mode: info.mode(),
        framebuffer: info.framebuffer(),
        position: info.position(),
        connector: connector_handle,
    };

    let (width, height) = mode.size();
    println!();
    println!("  {name}  {width}x{height}@{}", mode.vrefresh());
    println!("  showing a colour for {seconds}s, then restoring");
    println!();

    // Armed before master is taken. Everything after this point is inside the
    // watchdog's window, including the mode-set itself — which is the only
    // arrangement where a hang in mode-setting cannot strand the screen.
    let watchdog_card = Arc::clone(&card);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(seconds + 2));
        eprintln!("  watchdog fired — restoring and exiting");
        previous.restore(&watchdog_card);
        let _ = watchdog_card.release_master_lock();
        // Hard exit. The main thread is by definition not answering, so
        // unwinding would wait on the thing that is stuck.
        std::process::exit(2);
    });

    // Opened before master is taken, so a keyboard that cannot be reached
    // fails while the console is still readable rather than behind a blue
    // screen.
    let mut libinput = match open_libinput(&session, &seat) {
        Ok(libinput) => Some(libinput),
        Err(e) => {
            println!("  no keyboard ({e}); the watchdog is the only way out");
            None
        }
    };

    let result = show_colour(
        &card,
        &previous,
        connector_handle,
        crtc_handle,
        mode,
        seconds,
        libinput.as_mut(),
    );

    // The ordinary path out. The watchdog will also do this if it gets there
    // first; `set_crtc` twice with the same arguments is harmless.
    previous.restore(&card);
    let _ = card.release_master_lock();

    KEYS.with(|slot| {
        if let Some((keys, escaped)) = slot.borrow_mut().take() {
            if escaped {
                println!("  escape pressed — ended early");
            }
            match keys.len() {
                0 => println!("  no key events arrived (was anything typed?)"),
                n => println!("  {n} key press(es) seen, codes: {keys:?}"),
            }
        }
    });
    result
}

thread_local! {
    /// Carries what the input loop saw out to where it can be printed, which
    /// is after the mode is restored — anything written while the blue screen
    /// is up is written to a console nobody can read.
    static KEYS: std::cell::RefCell<Option<(Vec<u32>, bool)>> =
        const { std::cell::RefCell::new(None) };
}

fn show_colour(
    card: &Opened,
    previous: &Previous,
    connector: connector::Handle,
    crtc: crtc::Handle,
    mode: drm::control::Mode,
    seconds: u64,
    mut libinput: Option<&mut input::Libinput>,
) -> Result<(), String> {
    card.acquire_master_lock()
        .map_err(|e| format!("could not become DRM master: {e}\n  \
             Another compositor holds it — this needs its own VT."))?;

    let (width, height) = mode.size();
    let mut buffer = card
        .create_dumb_buffer((width as u32, height as u32), DrmFourcc::Xrgb8888, 32)
        .map_err(|e| format!("could not allocate a buffer: {e}"))?;

    {
        let mut mapping = card
            .map_dumb_buffer(&mut buffer)
            .map_err(|e| format!("could not map the buffer: {e}"))?;
        // HadalOS's own blue, so what appears on screen is unmistakably cusk
        // and not a leftover framebuffer. XRGB8888 is little-endian, so the
        // bytes go B, G, R, X.
        for pixel in mapping.as_mut().chunks_exact_mut(4) {
            pixel[0] = 0x1A;
            pixel[1] = 0x11;
            pixel[2] = 0x08;
            pixel[3] = 0x00;
        }
    }

    let fb = card
        .add_framebuffer(&buffer, 24, 32)
        .map_err(|e| format!("could not add a framebuffer: {e}"))?;

    let outcome = card
        .set_crtc(crtc, Some(fb), (0, 0), &[connector], Some(mode))
        .map_err(|e| format!("could not set the mode: {e}"));

    if outcome.is_ok() {
        // Polled rather than slept, so Escape can end it early. The watchdog
        // is still the guarantee; this is the way out that does not require
        // waiting for one.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(seconds);
        let mut keys = Vec::new();
        let mut escaped = false;
        while std::time::Instant::now() < deadline {
            if let Some(libinput) = libinput.as_deref_mut() {
                if pump(libinput, &mut keys) {
                    escaped = true;
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        // Printed after the mode is restored, further down, so it lands on a
        // console that can be read.
        KEYS.with(|slot| *slot.borrow_mut() = Some((keys, escaped)));
    }

    // Put the old mode back before tearing down the buffer it might otherwise
    // still be scanning out of.
    previous.restore(card);
    let _ = card.destroy_framebuffer(fb);
    let _ = card.destroy_dumb_buffer(buffer);
    outcome
}

// ── phase three: a keyboard ──────────────────────────────────────────────

/// Escape, in evdev codes. `KEY_ESC` is 1 and has been since Linux 1.0.
const KEY_ESC: u32 = 1;

/// Open libinput on this session's seat.
///
/// Through the session, like the DRM device: libinput needs `/dev/input/event*`
/// open, and logind hands those out to the session that holds control. Opening
/// them directly works as root and nowhere else, and a compositor that needs
/// root to read a keyboard is not a compositor anyone will run.
fn open_libinput(session: &LibSeatSession, seat: &str) -> Result<input::Libinput, String> {
    let mut libinput = input::Libinput::new_with_udev(LibinputSessionInterface::from(
        session.clone(),
    ));
    libinput
        .udev_assign_seat(seat)
        .map_err(|_| format!("libinput could not take seat {seat}"))?;
    Ok(libinput)
}

/// Drain libinput, reporting whether Escape was pressed.
///
/// Returns the keys seen as well, because "input works" and "input works *and
/// the right key arrived*" are different claims and only the second is worth
/// making.
fn pump(libinput: &mut input::Libinput, keys: &mut Vec<u32>) -> bool {
    use input::event::keyboard::KeyboardEventTrait;
    use input::event::{Event, KeyboardEvent};

    if libinput.dispatch().is_err() {
        return false;
    }
    let mut escape = false;
    for event in &mut *libinput {
        if let Event::Keyboard(KeyboardEvent::Key(key)) = event {
            if key.key_state() == input::event::keyboard::KeyState::Pressed {
                keys.push(key.key());
                if key.key() == KEY_ESC {
                    escape = true;
                }
            }
        }
    }
    escape
}

// ── phase three and a half: GL on the GPU cusk will actually use ─────────

/// Prove a `GlesRenderer` can be built on the DRM device through GBM, and that
/// it draws what it is told to.
///
/// The unknown standing in front of phase four. Everything on the tty so far
/// has used a **dumb buffer** — CPU memory the display controller scans out —
/// which says nothing about whether the GPU path works there. The render loop
/// needs GBM for allocation, EGL on the DRM node for a context, and a
/// `GlesRenderer` on top. If any of that fails it should fail here, in twenty
/// lines that can be run from a desktop, rather than in the middle of
/// restructuring the compositor's render loop.
///
/// Deliberately uses the **render node** (`renderD128`), which needs neither a
/// session nor DRM master — so unlike everything else in this module it runs
/// anywhere. Scanout is the part that needs the card node, and scanout is
/// already proven by the mode-set test.
pub fn probe_render() -> Result<String, String> {
    use smithay::backend::allocator::gbm::GbmDevice;
    use smithay::backend::egl::{EGLContext, EGLDisplay};
    use smithay::backend::renderer::gles::GlesRenderer;
    use smithay::backend::renderer::{Bind, Color32F, ExportMem, Frame, Offscreen, Renderer};
    use smithay::utils::{Rectangle, Size, Transform};

    let path = std::path::Path::new("/dev/dri/renderD128");
    let file = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
        .map_err(|e| format!("could not open {}: {e}", path.display()))?;

    let gbm = GbmDevice::new(file).map_err(|e| format!("no GBM device: {e}"))?;
    let display =
        unsafe { EGLDisplay::new(gbm) }.map_err(|e| format!("no EGL display on the GPU: {e}"))?;
    let context = EGLContext::new(&display).map_err(|e| format!("no EGL context: {e}"))?;
    let mut renderer =
        unsafe { GlesRenderer::new(context) }.map_err(|e| format!("no GL renderer: {e}"))?;

    // Small on purpose: this is a correctness check, not a benchmark. Two
    // sizes because allocation is in buffer coordinates and rendering is in
    // physical ones — the same numbers, different meanings, and smithay makes
    // the distinction in the type so it cannot be conflated by accident.
    let size = Size::<i32, smithay::utils::Buffer>::from((64, 64));
    let physical = Size::<i32, smithay::utils::Physical>::from((64, 64));
    let mut target: smithay::backend::renderer::gles::GlesTexture =
        Offscreen::create_buffer(&mut renderer, DrmFourcc::Abgr8888, size)
            .map_err(|e| format!("could not allocate a render target: {e}"))?;

    // A colour with all three channels distinct, so a channel swap shows up as
    // a wrong answer rather than as the same number twice.
    let expected = [0x11u8, 0x99, 0x33];
    {
        let mut framebuffer = renderer
            .bind(&mut target)
            .map_err(|e| format!("could not bind the target: {e}"))?;
        let mut frame = renderer
            .render(&mut framebuffer, physical, Transform::Normal)
            .map_err(|e| format!("could not start a frame: {e}"))?;
        frame
            .clear(
                Color32F::new(
                    expected[0] as f32 / 255.0,
                    expected[1] as f32 / 255.0,
                    expected[2] as f32 / 255.0,
                    1.0,
                ),
                &[Rectangle::from_size(physical)],
            )
            .map_err(|e| format!("could not clear: {e}"))?;
        let _ = frame.finish();
    }

    // Read it back. "The calls returned Ok" is not the same claim as "the
    // pixels are right", and only the second one is worth making — a context
    // on the wrong device can succeed at every call and render nothing.
    let framebuffer = renderer
        .bind(&mut target)
        .map_err(|e| format!("could not re-bind for readback: {e}"))?;
    let mapping = renderer
        .copy_framebuffer(&framebuffer, Rectangle::from_size(size), DrmFourcc::Abgr8888)
        .map_err(|e| format!("could not copy the framebuffer: {e}"))?;
    let pixels = renderer
        .map_texture(&mapping)
        .map_err(|e| format!("could not map the copy: {e}"))?;

    let got = pixels.get(0..3).ok_or("readback was empty")?;
    if got != expected {
        return Err(format!(
            "rendered the wrong colour: expected {expected:02X?}, got {got:02X?}"
        ));
    }

    // Allocating here as well, because the modifier a driver actually returns
    // is what decides the framebuffer flags — and getting those wrong is a
    // panic inside drm, not an error. Checking it on the render node costs
    // nothing and saves a trip to a VT to find out.
    let modifier_note = {
        use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags};
        use smithay::backend::allocator::{Allocator, Modifier};
        use smithay::reexports::drm::buffer::PlanarBuffer;

        let gbm = GbmDevice::new(
            std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .map_err(|e| format!("could not reopen for allocation: {e}"))?,
        )
        .map_err(|e| format!("no GBM device for allocation: {e}"))?;
        let mut allocator = GbmAllocator::new(gbm, GbmBufferFlags::RENDERING);
        match allocator.create_buffer(64, 64, DrmFourcc::Xrgb8888, &[Modifier::Linear]) {
            Ok(buffer) => {
                let modifier = PlanarBuffer::modifier(&buffer);
                format!(
                    "; a Linear allocation reports {modifier:?}, so framebuffer flags = {:?}",
                    framebuffer_flags(modifier)
                )
            }
            Err(e) => format!("; could not test allocation here ({e})"),
        }
    };

    Ok(format!(
        "{} — GBM, EGL and GlesRenderer all work, and the pixels are right{modifier_note}",
        path.display()
    ))
}

/// Which `FbCmd2Flags` a buffer's modifier calls for.
///
/// `add_planar_framebuffer` asserts that `MODIFIERS` is set exactly when the
/// buffer carries a real modifier — and *asserts*, so getting it wrong is a
/// panic rather than an error. `Invalid` counts as no modifier, which is the
/// part that is easy to miss: a driver can hand back `Invalid` for a buffer
/// that was allocated with an explicit modifier, and then the flag must be
/// unset even though a modifier was requested.
pub fn framebuffer_flags(
    modifier: Option<smithay::reexports::drm::buffer::DrmModifier>,
) -> smithay::reexports::drm::control::FbCmd2Flags {
    use smithay::reexports::drm::buffer::DrmModifier;
    use smithay::reexports::drm::control::FbCmd2Flags;

    match modifier {
        Some(modifier) if !matches!(modifier, DrmModifier::Invalid) => FbCmd2Flags::MODIFIERS,
        _ => FbCmd2Flags::empty(),
    }
}

// ── phase four groundwork: scanning out a GL-rendered buffer ─────────────

/// Render with the GPU and put *that* on the screen.
///
/// The last unproven link. `--modeset-test` scans out a **dumb buffer** — CPU
/// memory — and `--probe-render` renders with the GPU into an offscreen
/// texture nobody displays. Neither says the two halves join up, and joining
/// them is the whole of the DRM driver:
///
/// ```text
///   GbmAllocator -> GbmBuffer -> Dmabuf -> renderer.bind() -> draw
///                             -> add_planar_framebuffer -> set_crtc
/// ```
///
/// The same buffer is seen by the GPU as a render target and by the display
/// controller as a scanout source. If the format, the modifier or the flags
/// are wrong, one of those two rejects it — and finding out here costs a
/// bounded test rather than a stalled driver.
///
/// Single-buffered on purpose: this draws once and holds it. Double buffering
/// and page flips are the driver's problem, and adding them here would be
/// testing something this does not yet claim.
pub fn probe_scanout(seconds: u64) -> Result<(), String> {
    use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
    use smithay::backend::allocator::{Allocator, Modifier};
    use smithay::backend::egl::{EGLContext, EGLDisplay};
    use smithay::backend::renderer::gles::GlesRenderer;
    use smithay::backend::renderer::{Bind, Color32F, Frame, Renderer};
    use smithay::reexports::drm::buffer::PlanarBuffer;
    use smithay::utils::{Physical, Rectangle, Size, Transform};

    let (mut session, _notifier) = LibSeatSession::new().map_err(|e| {
        format!("could not join a session: {e}\n  This needs its own VT — see --probe-drm.")
    })?;

    let path = drm_devices().into_iter().next().ok_or("no DRM device")?;
    let fd = session
        .open(
            &path,
            rustix::fs::OFlags::RDWR | rustix::fs::OFlags::CLOEXEC | rustix::fs::OFlags::NONBLOCK,
        )
        .map_err(|e| format!("session refused {}: {e}", path.display()))?;
    let card = Arc::new(Opened { file: std::fs::File::from(fd) });

    let resources = card
        .resource_handles()
        .map_err(|e| format!("could not read resources: {e}"))?;
    let (connector_handle, mode) = resources
        .connectors()
        .iter()
        .filter_map(|handle| card.get_connector(*handle, true).ok())
        .filter(|info| info.state() == connector::State::Connected)
        .find_map(|info| info.modes().first().copied().map(|m| (info.handle(), m)))
        .ok_or("no connected output with a mode")?;
    let connector_info = card
        .get_connector(connector_handle, false)
        .map_err(|e| format!("could not re-read the connector: {e}"))?;
    let crtc_handle = connector_info
        .current_encoder()
        .and_then(|handle| card.get_encoder(handle).ok())
        .and_then(|encoder| encoder.crtc())
        .or_else(|| resources.crtcs().first().copied())
        .ok_or("no CRTC for that connector")?;

    let info = card
        .get_crtc(crtc_handle)
        .map_err(|e| format!("could not read the CRTC: {e}"))?;
    let previous = Previous {
        crtc: crtc_handle,
        mode: info.mode(),
        framebuffer: info.framebuffer(),
        position: info.position(),
        connector: connector_handle,
    };

    // The GPU side, on the card node rather than the render node: the buffer
    // has to be scannable by *this* display controller, and a buffer allocated
    // against a different device may be neither shareable nor scanout-capable.
    // Two GBM devices on two dups of the same card fd, because `EGLDisplay`
    // consumes one and the allocator needs the other. They address the same
    // hardware, which is what matters: a buffer from one is scannable by the
    // other.
    let dup = |what: &str| {
        card.file
            .try_clone()
            .map_err(|e| format!("could not dup the card for {what}: {e}"))
    };
    let gbm_for_egl = GbmDevice::new(dup("EGL")?).map_err(|e| format!("no GBM device: {e}"))?;
    let gbm_for_alloc =
        GbmDevice::new(dup("allocation")?).map_err(|e| format!("no GBM device: {e}"))?;
    let display = unsafe { EGLDisplay::new(gbm_for_egl) }
        .map_err(|e| format!("no EGL display: {e}"))?;
    let context = EGLContext::new(&display).map_err(|e| format!("no EGL context: {e}"))?;
    let mut renderer =
        unsafe { GlesRenderer::new(context) }.map_err(|e| format!("no GL renderer: {e}"))?;

    // SCANOUT is the flag that matters. Without it the allocation can succeed
    // and `add_planar_framebuffer` then refuses the buffer, which reads as a
    // mode-setting failure rather than an allocation one.
    let mut allocator = GbmAllocator::new(gbm_for_alloc, GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);

    let (width, height) = mode.size();
    let buffer = allocator
        .create_buffer(
            width as u32,
            height as u32,
            DrmFourcc::Xrgb8888,
            &[Modifier::Linear],
        )
        .map_err(|e| format!("could not allocate a scanout buffer: {e}"))?;

    let mut dmabuf = {
        use smithay::backend::allocator::dmabuf::AsDmabuf;
        buffer
            .export()
            .map_err(|e| format!("could not export the buffer as a dmabuf: {e}"))?
    };

    println!();
    println!(
        "  {:?}-{}  {width}x{height}@{}",
        connector_info.interface(),
        connector_info.interface_id(),
        mode.vrefresh()
    );
    println!("  rendering with the GPU into a scanout buffer for {seconds}s");
    println!();

    let watchdog_card = Arc::clone(&card);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(seconds + 2));
        eprintln!("  watchdog fired — restoring and exiting");
        previous.restore(&watchdog_card);
        let _ = watchdog_card.release_master_lock();
        std::process::exit(2);
    });

    card.acquire_master_lock()
        .map_err(|e| format!("could not become DRM master: {e}\n  This needs its own VT."))?;

    let outcome = (|| -> Result<(), String> {
        // The GPU writes here...
        {
            let mut framebuffer = renderer
                .bind(&mut dmabuf)
                .map_err(|e| format!("the renderer would not bind the dmabuf: {e}"))?;
            let size = Size::<i32, Physical>::from((width as i32, height as i32));
            let mut frame = renderer
                .render(&mut framebuffer, size, Transform::Normal)
                .map_err(|e| format!("could not start a frame: {e}"))?;
            // Distinct from the mode-set test's blue, so the two are
            // distinguishable on screen without reading the log.
            frame
                .clear(Color32F::new(0.07, 0.55, 0.52, 1.0), &[Rectangle::from_size(size)])
                .map_err(|e| format!("could not clear: {e}"))?;
            let _ = frame.finish();
        }

        // ...and the display controller reads from the same memory.
        //
        // The flags have to agree with the buffer: `add_planar_framebuffer`
        // asserts that `MODIFIERS` is set exactly when the buffer carries a
        // real modifier, and panics otherwise. Passing `empty()` with a
        // Linear-modifier buffer is what that panic was.
        //
        // Derived from the buffer with the same predicate the assertion uses,
        // rather than hardcoded, so the two cannot drift — and so a driver
        // that hands back `Invalid` instead of the requested modifier is
        // handled by the same line.
        let fb_flags = framebuffer_flags(PlanarBuffer::modifier(&buffer));
        let fb = card
            .add_planar_framebuffer(&buffer, fb_flags)
            .map_err(|e| format!("the display controller refused the buffer: {e}"))?;
        card.set_crtc(crtc_handle, Some(fb), (0, 0), &[connector_handle], Some(mode))
            .map_err(|e| format!("could not set the mode: {e}"))?;

        std::thread::sleep(std::time::Duration::from_secs(seconds));
        let _ = card.destroy_framebuffer(fb);
        Ok(())
    })();

    previous.restore(&card);
    let _ = card.release_master_lock();
    outcome
}

// ── phase four: the driver ───────────────────────────────────────────────

/// One scanout buffer and everything the two consumers of it need.
///
/// The GPU writes through the dmabuf; the display controller reads through the
/// framebuffer handle. Both refer to the same memory, and keeping them
/// together is what stops one being freed while the other is still using it.
pub struct Surface {
    dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
    framebuffer: framebuffer::Handle,
}

/// Everything the DRM driver owns for the life of a session.
pub struct Drm {
    card: Arc<Opened>,
    pub renderer: smithay::backend::renderer::gles::GlesRenderer,
    pub surfaces: [Surface; 2],
    /// Which surface is being scanned out. The other is the one to draw into.
    front: usize,
    pub crtc: crtc::Handle,
    pub connector: connector::Handle,
    pub mode: drm::control::Mode,
    previous: Previous,
    pub size: (i32, i32),
    /// Render formats, for the dmabuf global the driver registers itself.
    pub formats: Vec<smithay::backend::allocator::Format>,
}

impl Drm {
    /// Open the device, pick an output, and allocate a pair of buffers.
    ///
    /// Nothing here takes DRM master. That is deliberate: everything that can
    /// fail while the console is still readable should fail before the screen
    /// is taken over.
    pub fn open(session: &mut LibSeatSession) -> Result<Self, String> {
        use smithay::backend::allocator::dmabuf::AsDmabuf;
        use smithay::backend::allocator::gbm::{GbmAllocator, GbmBufferFlags, GbmDevice};
        use smithay::backend::allocator::{Allocator, Modifier};
        use smithay::backend::egl::{EGLContext, EGLDisplay};
        use smithay::backend::renderer::gles::GlesRenderer;
        use smithay::reexports::drm::buffer::PlanarBuffer;

        let path = drm_devices().into_iter().next().ok_or("no DRM device")?;
        let fd = session
            .open(
                &path,
                rustix::fs::OFlags::RDWR
                    | rustix::fs::OFlags::CLOEXEC
                    | rustix::fs::OFlags::NONBLOCK,
            )
            .map_err(|e| format!("session refused {}: {e}", path.display()))?;
        let card = Arc::new(Opened { file: std::fs::File::from(fd) });

        let resources = card
            .resource_handles()
            .map_err(|e| format!("could not read resources: {e}"))?;
        let (connector, mode) = resources
            .connectors()
            .iter()
            .filter_map(|handle| card.get_connector(*handle, true).ok())
            .filter(|info| info.state() == connector::State::Connected)
            .find_map(|info| info.modes().first().copied().map(|m| (info.handle(), m)))
            .ok_or("no connected output with a mode")?;
        let info = card
            .get_connector(connector, false)
            .map_err(|e| format!("could not re-read the connector: {e}"))?;
        let crtc = info
            .current_encoder()
            .and_then(|handle| card.get_encoder(handle).ok())
            .and_then(|encoder| encoder.crtc())
            .or_else(|| resources.crtcs().first().copied())
            .ok_or("no CRTC for that connector")?;

        let crtc_info = card
            .get_crtc(crtc)
            .map_err(|e| format!("could not read the CRTC: {e}"))?;
        let previous = Previous {
            crtc,
            mode: crtc_info.mode(),
            framebuffer: crtc_info.framebuffer(),
            position: crtc_info.position(),
            connector,
        };

        let dup = |what: &str| {
            card.file
                .try_clone()
                .map_err(|e| format!("could not dup the card for {what}: {e}"))
        };
        let gbm_for_egl = GbmDevice::new(dup("EGL")?).map_err(|e| format!("no GBM: {e}"))?;
        let gbm_for_alloc = GbmDevice::new(dup("allocation")?).map_err(|e| format!("no GBM: {e}"))?;
        let display =
            unsafe { EGLDisplay::new(gbm_for_egl) }.map_err(|e| format!("no EGL display: {e}"))?;
        let formats = display.dmabuf_render_formats().iter().copied().collect();
        let context = EGLContext::new(&display).map_err(|e| format!("no EGL context: {e}"))?;
        let renderer =
            unsafe { GlesRenderer::new(context) }.map_err(|e| format!("no GL renderer: {e}"))?;

        let (width, height) = mode.size();
        let mut allocator =
            GbmAllocator::new(gbm_for_alloc, GbmBufferFlags::RENDERING | GbmBufferFlags::SCANOUT);

        // Two, so a frame is never drawn into the buffer the display is
        // currently reading. One buffer means every frame is visible while it
        // is still being assembled.
        let mut make = || -> Result<Surface, String> {
            let buffer = allocator
                .create_buffer(width as u32, height as u32, DrmFourcc::Xrgb8888, &[Modifier::Linear])
                .map_err(|e| format!("could not allocate a scanout buffer: {e}"))?;
            let framebuffer = card
                .add_planar_framebuffer(&buffer, framebuffer_flags(PlanarBuffer::modifier(&buffer)))
                .map_err(|e| format!("the display controller refused the buffer: {e}"))?;
            let dmabuf = buffer
                .export()
                .map_err(|e| format!("could not export the buffer: {e}"))?;
            Ok(Surface { dmabuf, framebuffer })
        };
        let surfaces = [make()?, make()?];

        Ok(Drm {
            card,
            renderer,
            surfaces,
            front: 0,
            crtc,
            connector,
            mode,
            previous,
            size: (width as i32, height as i32),
            formats,
        })
    }

    /// Take the display, if it is not already ours.
    ///
    /// **Not fatal when it fails.** With logind, `TakeDevice` on an active
    /// session already returns a master-capable fd — logind does the granting,
    /// and a process calling `SET_MASTER` itself needs root. So `EACCES` here
    /// usually means "you are already master and did not need to ask", not
    /// "you cannot have the display".
    ///
    /// Treating it as fatal is what made `--tty` refuse to start as an
    /// ordinary user and demand `sudo` — which then failed differently,
    /// because sudo strips `XDG_RUNTIME_DIR` and the Wayland socket cannot be
    /// created without it. The real test is whether `set_crtc` works, and that
    /// reports its own error a moment later.
    pub fn take_display(&self) {
        if let Err(e) = self.card.acquire_master_lock() {
            tracing::debug!("set_master declined ({e}); assuming logind already granted it");
        }
    }

    /// Draw into the buffer that is not currently on screen.
    ///
    /// A closure rather than handing out the renderer and the buffer
    /// separately, because both are fields of this struct and the caller
    /// cannot borrow two of them at once. Destructuring here splits the borrow
    /// where the compiler can see it.
    pub fn with_back<T>(
        &mut self,
        draw: impl FnOnce(
            &mut smithay::backend::renderer::gles::GlesRenderer,
            &mut <smithay::backend::renderer::gles::GlesRenderer as smithay::backend::renderer::RendererSuper>::Framebuffer<'_>,
        ) -> T,
    ) -> Result<T, String> {
        use smithay::backend::renderer::Bind;

        let back = 1 - self.front;
        let Drm { renderer, surfaces, .. } = self;
        let mut framebuffer = renderer
            .bind(&mut surfaces[back].dmabuf)
            .map_err(|e| format!("could not bind the scanout buffer: {e}"))?;
        Ok(draw(renderer, &mut framebuffer))
    }

    /// Show what was just drawn.
    ///
    /// `set_crtc` rather than a page flip. A flip is asynchronous and its
    /// completion arrives as a DRM event that has to be read before the next
    /// one can be queued; doing that properly needs the event loop that this
    /// driver does not have yet. `set_crtc` is synchronous and tears, which is
    /// visible and honest, where a flip queued twice without draining silently
    /// returns `EBUSY` and the screen simply stops updating.
    /// Returns the raw error, because the caller must distinguish *this is not
    /// our display right now* from *this is broken*. Collapsing both to a
    /// string made a VT switch look like a fatal fault, and cusk exited on it.
    pub fn present(&mut self) -> std::io::Result<()> {
        let back = 1 - self.front;
        self.card.set_crtc(
            self.crtc,
            Some(self.surfaces[back].framebuffer),
            (0, 0),
            &[self.connector],
            Some(self.mode),
        )?;
        self.front = back;
        Ok(())
    }

    /// Whether an error means the session simply does not own the hardware.
    ///
    /// logind revokes device access the instant a VT switch begins, but
    /// `PauseSession` only arrives on the next notifier dispatch. Every frame
    /// in that window fails with `EACCES`, and it is not a fault — it is the
    /// switch, observed before the notification.
    pub fn is_revoked(error: &std::io::Error) -> bool {
        matches!(
            error.kind(),
            std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::NotConnected
        )
    }

    pub fn restore(&self) {
        self.previous.restore(&self.card);
        let _ = self.card.release_master_lock();
    }

    /// Arm the watchdog. Call before taking the display.
    pub fn arm_watchdog(&self, seconds: u64) {
        let card = Arc::clone(&self.card);
        let previous = self.previous;
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(seconds));
            eprintln!("  watchdog fired — restoring and exiting");
            previous.restore(&card);
            let _ = card.release_master_lock();
            std::process::exit(2);
        });
    }
}

/// Open libinput for a session, for the driver to pump.
pub fn libinput_for(session: &LibSeatSession, seat: &str) -> Result<input::Libinput, String> {
    open_libinput(session, seat)
}


/// What draining libinput produced.
#[derive(Default)]
pub struct Input {
    pub escape: bool,
    /// Key presses and releases, as evdev codes.
    pub keys: Vec<(u32, bool)>,
    /// Accumulated pointer movement since the last drain.
    pub motion: (f64, f64),
    pub buttons: Vec<(u32, bool)>,
    /// One entry per libinput scroll event, not accumulated: the source and
    /// the stop flags are per-event, and summing them would merge a wheel
    /// notch with a finger lift.
    pub scrolls: Vec<Scroll>,
}

/// Drain libinput once.
///
/// Returns what happened rather than calling back into the compositor, because
/// the caller needs `&mut Cusk` for the seat and libinput is borrowed here —
/// two mutable borrows otherwise, and the shape that avoids it is a plain
/// value.
///
/// Motion is **accumulated**, not reported per event. A touchpad emits dozens
/// of deltas between frames, and dispatching each one separately would mean a
/// hit test and an enter/leave pass per delta for a pointer that only ends up
/// somewhere once.
pub fn drain(libinput: &mut input::Libinput) -> Input {
    use input::event::keyboard::KeyboardEventTrait;
    use input::event::{Event, KeyboardEvent, PointerEvent};

    let mut out = Input::default();
    if libinput.dispatch().is_err() {
        return out;
    }
    for event in &mut *libinput {
        match event {
            Event::Keyboard(KeyboardEvent::Key(key)) => {
                let pressed = key.key_state() == input::event::keyboard::KeyState::Pressed;
                if pressed && key.key() == KEY_ESC {
                    out.escape = true;
                }
                out.keys.push((key.key(), pressed));
            }
            Event::Pointer(PointerEvent::Motion(motion)) => {
                out.motion.0 += motion.dx();
                out.motion.1 += motion.dy();
            }
            // Absolute devices — tablets, touchscreens — report where they
            // *are*. Not handled yet rather than handled wrongly: mapping them
            // needs the device's own coordinate range, and treating an
            // absolute position as a delta would fling the pointer across the
            // screen on every touch.
            Event::Pointer(PointerEvent::ScrollWheel(scroll)) => {
                use input::event::pointer::{Axis, PointerScrollEvent};
                out.scrolls.push(Scroll {
                    source: ScrollSource::Wheel,
                    horizontal: scroll.scroll_value(Axis::Horizontal),
                    vertical: scroll.scroll_value(Axis::Vertical),
                    // A wheel's discrete steps are what a client needs to
                    // scroll "one notch"; the smooth value alone makes every
                    // wheel behave like a trackpad.
                    v120: Some((
                        scroll.scroll_value_v120(Axis::Horizontal),
                        scroll.scroll_value_v120(Axis::Vertical),
                    )),
                });
            }
            Event::Pointer(PointerEvent::ScrollFinger(scroll)) => {
                use input::event::pointer::{Axis, PointerScrollEvent};
                out.scrolls.push(Scroll {
                    source: ScrollSource::Finger,
                    horizontal: scroll.scroll_value(Axis::Horizontal),
                    vertical: scroll.scroll_value(Axis::Vertical),
                    v120: None,
                });
            }
            Event::Pointer(PointerEvent::ScrollContinuous(scroll)) => {
                use input::event::pointer::{Axis, PointerScrollEvent};
                out.scrolls.push(Scroll {
                    source: ScrollSource::Continuous,
                    horizontal: scroll.scroll_value(Axis::Horizontal),
                    vertical: scroll.scroll_value(Axis::Vertical),
                    v120: None,
                });
            }
            Event::Pointer(PointerEvent::Button(button)) => {
                let pressed =
                    button.button_state() == input::event::pointer::ButtonState::Pressed;
                out.buttons.push((button.button(), pressed));
            }
            _ => {}
        }
    }
    out
}

/// Move a pointer by a delta and keep it on screen.
///
/// Clamped to the output. A pointer that can leave the screen cannot be
/// brought back — there is no desktop edge to catch it and no other compositor
/// to reset it — so this is the difference between a usable session and one
/// that has to be killed from another VT.
pub fn clamp_pointer(
    at: (f64, f64),
    delta: (f64, f64),
    output: (i32, i32),
) -> (f64, f64) {
    // One pixel short of the far edge: a pointer exactly at `width` is outside
    // every window, so the rightmost column would never be clickable.
    let max_x = (output.0 as f64 - 1.0).max(0.0);
    let max_y = (output.1 as f64 - 1.0).max(0.0);
    (
        (at.0 + delta.0).clamp(0.0, max_x),
        (at.1 + delta.1).clamp(0.0, max_y),
    )
}

/// Whether the session currently owns the hardware.
///
/// logind revokes device access when the user switches to another VT, and
/// restores it on switching back. A compositor that keeps drawing through a
/// switch is writing to a revoked fd: every frame fails, and the errors arrive
/// on a console the user is no longer looking at.
#[derive(Default)]
pub struct Active {
    pub active: bool,
    /// Set on the transition, so the driver can do the work that only makes
    /// sense once — reclaiming input, forcing a mode-set — rather than on
    /// every frame while active.
    pub just_resumed: bool,
}

/// Watch for VT switches.
///
/// A calloop loop dispatched with a zero timeout each frame, rather than the
/// whole driver restructured around calloop. The notifier is an event source
/// and this is the smallest thing that can poll one; moving the render loop
/// onto calloop is worth doing when DRM page-flip events need draining, and
/// not before.
pub fn watch_session(
    notifier: smithay::backend::session::libseat::LibSeatSessionNotifier,
) -> Result<
    (
        smithay::reexports::calloop::EventLoop<'static, Active>,
        Active,
    ),
    String,
> {
    use smithay::backend::session::Event;
    use smithay::reexports::calloop::EventLoop;

    let event_loop: EventLoop<Active> =
        EventLoop::try_new().map_err(|e| format!("could not create an event loop: {e}"))?;
    event_loop
        .handle()
        .insert_source(notifier, |event, _, active: &mut Active| match event {
            Event::PauseSession => {
                tracing::info!("session paused — another VT has the display");
                active.active = false;
            }
            Event::ActivateSession => {
                tracing::info!("session resumed");
                active.active = true;
                active.just_resumed = true;
            }
        })
        .map_err(|e| format!("could not watch the session: {e}"))?;

    // Starts active: cusk only gets this far on the VT it was launched from,
    // and waiting for an activate that has already happened would hang before
    // the first frame.
    Ok((event_loop, Active { active: true, just_resumed: false }))
}

/// evdev codes for the modifiers that arm a VT switch, and the function keys.
const KEY_LEFTCTRL: u32 = 29;
const KEY_RIGHTCTRL: u32 = 97;
const KEY_LEFTALT: u32 = 56;
const KEY_RIGHTALT: u32 = 100;
const KEY_F1: u32 = 59;
const KEY_F10: u32 = 68;
const KEY_F11: u32 = 87;
const KEY_F12: u32 = 88;

/// Which VT `Ctrl+Alt+F<n>` asks for, if this key is a function key.
///
/// Raw evdev codes rather than xkb keysyms. The keysym route depends on the
/// layout including `srvr_ctrl(fkey2vt)`, and when it does not, `XF86Switch_VT`
/// never arrives and VT switching silently does not work — which is exactly
/// the failure that is hardest to attribute. Switching terminals is a physical
/// -key operation, so a physical key is the right thing to read.
///
/// F11 and F12 are not adjacent to F1..F10 in evdev, which is the off-by-many
/// this exists to contain.
pub fn vt_for_key(code: u32) -> Option<i32> {
    match code {
        KEY_F1..=KEY_F10 => Some((code - KEY_F1 + 1) as i32),
        KEY_F11 => Some(11),
        KEY_F12 => Some(12),
        _ => None,
    }
}

/// Tracks whether a VT-switch chord is armed.
///
/// Kept here rather than read from the compositor's xkb state, because a VT
/// switch has to work even when the keyboard focus is somewhere that would
/// swallow it — and because the compositor's modifier state is updated by the
/// same keys this is watching, which makes the ordering a coin toss.
#[derive(Default)]
pub struct Chord {
    ctrl: bool,
    alt: bool,
}

impl Chord {
    /// Feed a key. Returns the VT to switch to, if this completes the chord.
    pub fn key(&mut self, code: u32, pressed: bool) -> Option<i32> {
        match code {
            KEY_LEFTCTRL | KEY_RIGHTCTRL => {
                self.ctrl = pressed;
                None
            }
            KEY_LEFTALT | KEY_RIGHTALT => {
                self.alt = pressed;
                None
            }
            // On press only. Acting on the release as well would switch away
            // and immediately back.
            code if pressed && self.ctrl && self.alt => vt_for_key(code),
            _ => None,
        }
    }
}

/// One scroll event, in the terms `AxisFrame` needs.
///
/// Kept as data rather than turned into an `AxisFrame` here, because building
/// one needs smithay types that this module deliberately does not carry — and
/// because it makes the mapping decisions inspectable rather than buried in a
/// chain of builder calls.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Scroll {
    pub source: ScrollSource,
    /// Pixels. Positive is right and down, matching pointer motion.
    pub horizontal: f64,
    pub vertical: f64,
    /// Discrete steps, in 120ths of a notch. Wheels only.
    pub v120: Option<(f64, f64)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollSource {
    Wheel,
    Finger,
    Continuous,
}

impl Scroll {
    /// Whether a finger has lifted, per axis.
    ///
    /// libinput ends a touchpad scroll with a zero on the axis that stopped,
    /// and a client needs that to end kinetic scrolling. Without it a
    /// touchpad flick keeps coasting in applications that implement momentum,
    /// which reads as the scroll being stuck.
    pub fn stopped(&self) -> (bool, bool) {
        if self.source != ScrollSource::Finger {
            return (false, false);
        }
        (self.horizontal == 0.0, self.vertical == 0.0)
    }

    /// Whether this event says anything at all.
    ///
    /// A frame with no axes in it is a message a client cannot act on, and
    /// some toolkits treat one as a scroll stop — so an empty event must be
    /// dropped rather than forwarded.
    pub fn is_empty(&self) -> bool {
        self.horizontal == 0.0
            && self.vertical == 0.0
            && self.source != ScrollSource::Finger
    }
}

#[cfg(test)]
mod tests {
    use super::framebuffer_flags;
    use smithay::reexports::drm::buffer::DrmModifier;
    use smithay::reexports::drm::control::FbCmd2Flags;

    /// A real modifier means the flag must be set, or `add_planar_framebuffer`
    /// asserts and the process dies.
    /// A pointer that can leave the screen cannot be brought back: there is no
    /// desktop edge to catch it and no other compositor to reset it. This is
    /// the difference between a usable session and one killed from another VT.
    #[test]
    fn the_pointer_cannot_leave_the_screen() {
        let output = (1920, 1080);
        assert_eq!(super::clamp_pointer((0.0, 0.0), (-500.0, -500.0), output), (0.0, 0.0));
        let far = super::clamp_pointer((1900.0, 1000.0), (500.0, 500.0), output);
        assert!(far.0 <= 1919.0 && far.1 <= 1079.0, "{far:?}");
    }

    /// One pixel short of the far edge, because a pointer exactly at `width`
    /// is outside every window and the rightmost column would never respond.
    #[test]
    fn the_far_edge_stays_clickable() {
        let at = super::clamp_pointer((0.0, 0.0), (9999.0, 9999.0), (1920, 1080));
        assert_eq!(at, (1919.0, 1079.0));
    }

    #[test]
    fn ordinary_motion_is_just_added() {
        assert_eq!(super::clamp_pointer((100.0, 100.0), (5.5, -3.0), (1920, 1080)), (105.5, 97.0));
    }

    /// A degenerate output must not produce a negative bound, which would make
    /// `clamp` panic on an inverted range.
    #[test]
    fn a_zero_sized_output_does_not_panic() {
        assert_eq!(super::clamp_pointer((0.0, 0.0), (10.0, 10.0), (0, 0)), (0.0, 0.0));
    }

    /// F11 and F12 are not adjacent to F1..F10 in evdev, so a single
    /// subtraction gets them wrong — and the symptom is switching to the wrong
    /// terminal, which looks like a broken keyboard.
    /// The distinction that keeps a VT switch from looking like a crash.
    fn wheel(h: f64, v: f64) -> super::Scroll {
        super::Scroll {
            source: super::ScrollSource::Wheel,
            horizontal: h,
            vertical: v,
            v120: Some((h * 120.0, v * 120.0)),
        }
    }

    fn finger(h: f64, v: f64) -> super::Scroll {
        super::Scroll {
            source: super::ScrollSource::Finger,
            horizontal: h,
            vertical: v,
            v120: None,
        }
    }

    /// A frame with no axes is a message a client cannot act on, and some
    /// toolkits read one as a scroll stop.
    #[test]
    fn an_empty_wheel_event_is_dropped() {
        assert!(wheel(0.0, 0.0).is_empty());
        assert!(!wheel(0.0, 1.0).is_empty());
        assert!(!wheel(-1.0, 0.0).is_empty());
    }

    /// A finger event with zeros is not empty — it is the lift, and it is the
    /// one scroll event that carries information by being zero.
    #[test]
    fn a_finger_lift_is_not_an_empty_event() {
        assert!(!finger(0.0, 0.0).is_empty());
        assert_eq!(finger(0.0, 0.0).stopped(), (true, true));
    }

    /// Stopping one axis must not stop the other, or a diagonal flick ends
    /// sideways.
    #[test]
    fn each_axis_stops_independently() {
        assert_eq!(finger(0.0, 5.0).stopped(), (true, false));
        assert_eq!(finger(5.0, 0.0).stopped(), (false, true));
        assert_eq!(finger(5.0, 5.0).stopped(), (false, false));
    }

    /// Only a touchpad reports lifts. A wheel at rest simply sends nothing,
    /// and calling that a stop would end kinetic scrolling that a finger
    /// started.
    #[test]
    fn only_a_finger_reports_a_stop() {
        assert_eq!(wheel(0.0, 0.0).stopped(), (false, false));
        assert_eq!(
            super::Scroll {
                source: super::ScrollSource::Continuous,
                horizontal: 0.0,
                vertical: 0.0,
                v120: None,
            }
            .stopped(),
            (false, false)
        );
    }

    #[test]
    fn a_revoked_device_is_not_a_fault() {
        use std::io::{Error, ErrorKind};
        assert!(super::Drm::is_revoked(&Error::from(ErrorKind::PermissionDenied)));
        assert!(super::Drm::is_revoked(&Error::from(ErrorKind::NotConnected)));
    }

    /// A real failure must still be one, or a broken device would look like a
    /// VT switch and cusk would spin forever pretending to be paused.
    #[test]
    fn other_errors_are_still_faults() {
        use std::io::{Error, ErrorKind};
        for kind in [ErrorKind::NotFound, ErrorKind::InvalidInput, ErrorKind::Other] {
            assert!(!super::Drm::is_revoked(&Error::from(kind)), "{kind:?}");
        }
    }

    #[test]
    fn function_keys_map_to_the_terminal_they_name() {
        assert_eq!(super::vt_for_key(59), Some(1));
        assert_eq!(super::vt_for_key(60), Some(2));
        assert_eq!(super::vt_for_key(68), Some(10));
        assert_eq!(super::vt_for_key(87), Some(11));
        assert_eq!(super::vt_for_key(88), Some(12));
    }

    #[test]
    fn other_keys_are_not_terminals() {
        for code in [1, 30, 57, 69, 86, 89, 200] {
            assert_eq!(super::vt_for_key(code), None, "code {code}");
        }
    }

    /// Both modifiers, or an ordinary F5 in an editor would switch terminals.
    #[test]
    fn the_chord_needs_ctrl_and_alt() {
        let mut chord = super::Chord::default();
        assert_eq!(chord.key(59, true), None, "F1 alone");

        chord.key(29, true);
        assert_eq!(chord.key(59, true), None, "ctrl alone");

        chord.key(56, true);
        assert_eq!(chord.key(59, true), Some(1), "ctrl+alt+F1");
    }

    /// Releasing a modifier disarms it, or the chord stays live for the rest
    /// of the session and every F-key becomes a VT switch.
    #[test]
    fn releasing_a_modifier_disarms_the_chord() {
        let mut chord = super::Chord::default();
        chord.key(29, true);
        chord.key(56, true);
        assert_eq!(chord.key(60, true), Some(2));

        chord.key(56, false);
        assert_eq!(chord.key(60, true), None, "alt released");
    }

    /// Acting on release as well would switch away and immediately back.
    #[test]
    fn only_the_press_switches() {
        let mut chord = super::Chord::default();
        chord.key(29, true);
        chord.key(56, true);
        assert_eq!(chord.key(59, true), Some(1));
        assert_eq!(chord.key(59, false), None, "release must do nothing");
    }

    /// The right-hand modifiers are the same chord.
    #[test]
    fn either_side_of_the_keyboard_works() {
        let mut chord = super::Chord::default();
        chord.key(97, true);
        chord.key(100, true);
        assert_eq!(chord.key(61, true), Some(3));
    }

    #[test]
    fn a_real_modifier_sets_the_flag() {
        assert_eq!(
            framebuffer_flags(Some(DrmModifier::Linear)),
            FbCmd2Flags::MODIFIERS
        );
    }

    /// `Invalid` is the trap. It is `Some`, so a naive `is_some()` sets the
    /// flag — and then the assertion fails because drm filters `Invalid` out
    /// before comparing.
    #[test]
    fn an_invalid_modifier_counts_as_none() {
        assert_eq!(framebuffer_flags(Some(DrmModifier::Invalid)), FbCmd2Flags::empty());
    }

    #[test]
    fn no_modifier_leaves_the_flag_clear() {
        assert_eq!(framebuffer_flags(None), FbCmd2Flags::empty());
    }
}
