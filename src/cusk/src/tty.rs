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
        if let Err(e) = card.set_crtc(
            self.crtc,
            self.framebuffer,
            self.position,
            &[self.connector],
            self.mode,
        ) {
            eprintln!("  could not restore the previous mode: {e}");
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

    /// Take the display. Everything before this point is reversible.
    pub fn take_display(&self) -> Result<(), String> {
        self.card
            .acquire_master_lock()
            .map_err(|e| format!("could not become DRM master: {e}\n  This needs its own VT."))
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
    pub fn present(&mut self) -> Result<(), String> {
        let back = 1 - self.front;
        self.card
            .set_crtc(
                self.crtc,
                Some(self.surfaces[back].framebuffer),
                (0, 0),
                &[self.connector],
                Some(self.mode),
            )
            .map_err(|e| format!("could not present: {e}"))?;
        self.front = back;
        Ok(())
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

/// Drain libinput, feeding keys to the compositor and reporting Escape.
///
/// Keyboard only, for now. The pointer needs absolute positioning that
/// libinput does not provide for relative devices — the driver has to
/// integrate motion itself and clamp to the output — and doing that badly
/// means a cursor that drifts off screen and cannot be brought back.
pub fn pump_keyboard<F>(libinput: &mut input::Libinput, mut on_key: F) -> bool
where
    F: FnMut(u32, bool),
{
    use input::event::keyboard::KeyboardEventTrait;
    use input::event::{Event, KeyboardEvent};

    if libinput.dispatch().is_err() {
        return false;
    }
    let mut escape = false;
    for event in &mut *libinput {
        if let Event::Keyboard(KeyboardEvent::Key(key)) = event {
            let pressed = key.key_state() == input::event::keyboard::KeyState::Pressed;
            if pressed && key.key() == KEY_ESC {
                escape = true;
            }
            on_key(key.key(), pressed);
        }
    }
    escape
}

#[cfg(test)]
mod tests {
    use super::framebuffer_flags;
    use smithay::reexports::drm::buffer::DrmModifier;
    use smithay::reexports::drm::control::FbCmd2Flags;

    /// A real modifier means the flag must be set, or `add_planar_framebuffer`
    /// asserts and the process dies.
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
