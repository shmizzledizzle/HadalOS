//! `cusk-lock` — the session locker.
//!
//! Locks the session through `ext-session-lock-v1`, draws an indicator, and
//! unlocks when PAM says the password is right. That is the whole program.
//!
//! # Why there is no text on screen
//!
//! The compositor has a font rasteriser, and it is not used here. `cusk::text`
//! renders into `cusk::wallpaper::Image`, a type in the compositor binary that
//! pulls smithay and the image crate behind it — so borrowing the rasteriser
//! means dragging both into the process that handles your password.
//!
//! That trade is worth refusing. This binary sees the plaintext of a
//! credential; every crate linked into it is code that could see it too. So the
//! indicator is geometry — a bar that fills as you type, coloured by state —
//! which needs no font and no decoder, and the dependency list stays at three
//! things that are all load-bearing. It is also what swaylock's default looks
//! like, so it is not an unfamiliar shape.
//!
//! # Not locking is a valid outcome
//!
//! `auth::preflight` runs before the lock request. If PAM cannot initialise,
//! this exits having locked nothing. See `auth.rs` — the asymmetry between the
//! two ways a locker can be wrong is the thing that module is built around.

mod auth;

use std::time::Duration;

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    delegate_compositor, delegate_keyboard, delegate_output, delegate_registry, delegate_seat,
    delegate_session_lock, delegate_shm,
    output::{OutputHandler, OutputState},
    reexports::calloop::EventLoop,
    reexports::calloop_wayland_source::WaylandSource,
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
        Capability, SeatHandler, SeatState,
    },
    session_lock::{
        SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface,
        SessionLockSurfaceConfigure,
    },
    shm::{raw::RawPool, Shm, ShmHandler},
};
use wayland_client::{
    globals::registry_queue_init,
    protocol::{wl_keyboard, wl_output, wl_seat, wl_shm, wl_surface},
    Connection, QueueHandle,
};

/// How the indicator is drawn right now.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    /// Nothing typed yet.
    Idle,
    /// Characters entered.
    Typing,
    /// PAM is being asked.
    ///
    /// Distinct from `Typing` because `verify` blocks — `unix_chkpwd` is a
    /// process spawn and `pam_unix` deliberately delays a failure by a second
    /// or more. Without a visible state change the screen simply stops
    /// responding at the moment the user most wants to know it heard them.
    Verifying,
    /// PAM said no.
    Wrong,
}

impl State {
    fn colour(self) -> cusk::theme::Rgba {
        match self {
            State::Idle => cusk::theme::SURFACE_HI,
            State::Typing => cusk::theme::ACCENT,
            State::Verifying => cusk::theme::WARNING,
            State::Wrong => cusk::theme::DANGER,
        }
    }
}

struct Lock {
    conn: Connection,
    compositor_state: CompositorState,
    output_state: OutputState,
    registry_state: RegistryState,
    seat_state: SeatState,
    shm: Shm,
    session_lock_state: SessionLockState,
    session_lock: Option<SessionLock>,
    surfaces: Vec<SessionLockSurface>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    authenticator: auth::Authenticator,
    qh: QueueHandle<Lock>,
    /// Configured size per lock surface. A surface cannot be drawn before
    /// its first configure, and each output may differ.
    sizes: Vec<(wl_surface::WlSurface, (u32, u32))>,

    /// The typed password.
    ///
    /// Zeroed on every path that discards it — see `clear`. Rust will not do
    /// this: `String`'s destructor frees the allocation without touching the
    /// bytes, so a correct-looking program leaves the password in the heap for
    /// whatever allocates next.
    password: String,
    state: State,
    exit: bool,
}

fn main() {
    // Before anything is locked. An error here is a message on a terminal; the
    // same error after locking is a screen nobody can dismiss.
    let authenticator = match auth::Authenticator::preflight() {
        Ok(authenticator) => authenticator,
        Err(problem) => {
            eprintln!("cusk-lock: {problem}");
            eprintln!("cusk-lock: nothing was locked.");
            std::process::exit(1);
        }
    };

    // Stated before locking, on the terminal that started this. It is the
    // only chance to see which account and which PAM stack the screen will
    // check against — afterwards there is nowhere to print it, and "it rejects
    // my password" is unanswerable without knowing whose password it wanted.
    eprintln!(
        "cusk-lock: will authenticate {} via PAM service {}",
        authenticator.user(),
        authenticator.service()
    );

    let conn = match Connection::connect_to_env() {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("cusk-lock: no Wayland display ({e}); nothing was locked.");
            std::process::exit(1);
        }
    };

    let (globals, event_queue) = registry_queue_init(&conn).expect("registry");
    let qh: QueueHandle<Lock> = event_queue.handle();
    let mut event_loop: EventLoop<Lock> = EventLoop::try_new().expect("event loop");

    let mut lock = Lock {
        compositor_state: CompositorState::bind(&globals, &qh).expect("wl_compositor"),
        output_state: OutputState::new(&globals, &qh),
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        shm: Shm::bind(&globals, &qh).expect("wl_shm"),
        session_lock_state: SessionLockState::new(&globals, &qh),
        session_lock: None,
        surfaces: Vec::new(),
        keyboard: None,
        authenticator,
        password: String::new(),
        state: State::Idle,
        exit: false,
        conn: conn.clone(),
        qh: qh.clone(),
        sizes: Vec::new(),
    };

    lock.session_lock = match lock.session_lock_state.lock(&qh) {
        Ok(session_lock) => Some(session_lock),
        Err(e) => {
            eprintln!("cusk-lock: this compositor does not support ext-session-lock-v1 ({e}).");
            eprintln!("cusk-lock: nothing was locked.");
            std::process::exit(1);
        }
    };

    // One surface per output, or the outputs without one show whatever the
    // compositor puts there — which must be black, but is not this program's
    // indicator, so the machine looks half-locked.
    for output in lock.output_state.outputs() {
        let surface = lock.compositor_state.create_surface(&qh);
        let lock_surface =
            lock.session_lock.as_ref().unwrap().create_lock_surface(surface, &output, &qh);
        lock.surfaces.push(lock_surface);
    }

    WaylandSource::new(conn, event_queue).insert(event_loop.handle()).expect("wayland source");

    while !lock.exit {
        if event_loop.dispatch(Duration::from_millis(16), &mut lock).is_err() {
            // The compositor went away. The session went with it, so there is
            // nothing left to unlock and nothing to report to.
            break;
        }
    }
}

impl Lock {
    /// Discard the password, leaving no copy behind.
    ///
    /// Overwritten before being truncated. `String::clear` sets the length to
    /// zero and leaves the bytes in the allocation; `drop` frees the allocation
    /// without touching them either. Neither is enough for a credential, and
    /// the cost here is a memset of at most a few dozen bytes.
    fn clear(&mut self) {
        // SAFETY-adjacent: the bytes are overwritten in place and the string is
        // then truncated, so no invalid UTF-8 is ever observable.
        unsafe {
            for byte in self.password.as_mut_vec().iter_mut() {
                *byte = 0;
            }
        }
        self.password.clear();
    }

    fn attempt(&mut self) {
        if self.password.is_empty() {
            return;
        }
        // Painted before the blocking call, and flushed, or the state change is
        // queued behind a verify that takes a second and arrives after it.
        self.state = State::Verifying;
        self.redraw();
        let _ = self.conn.flush();

        let ok = self.authenticator.verify(&self.password);
        self.clear();

        if ok {
            // Unlock, then round-trip *before* exiting. Dropping the connection
            // with the request still queued leaves the compositor locked with
            // no client — which the protocol requires it to honour, so the
            // session would stay locked forever.
            if let Some(session_lock) = self.session_lock.take() {
                session_lock.unlock();
                let _ = self.conn.roundtrip();
            }
            self.exit = true;
        } else {
            self.state = State::Wrong;
            self.redraw();
        }
    }

    fn redraw(&self) {
        for surface in &self.surfaces {
            self.draw(surface);
        }
    }

    fn draw(&self, surface: &SessionLockSurface) {
        let Some((width, height)) = self.size_of(surface) else { return };
        if width == 0 || height == 0 {
            return;
        }

        let Ok(mut pool) = RawPool::new(width as usize * height as usize * 4, &self.shm) else {
            return;
        };
        let canvas = pool.mmap();

        let background = pack(cusk::theme::BG);
        for chunk in canvas.chunks_exact_mut(4) {
            chunk.copy_from_slice(&background.to_le_bytes());
        }

        // A bar rather than a ring: a filled rectangle is a handful of
        // arithmetic and a circle is not, and nothing here is worth an
        // anti-aliasing routine in the process holding a password.
        let bar_w = (width / 5).clamp(120, 480);
        let bar_h = 8u32;
        let x0 = (width.saturating_sub(bar_w)) / 2;
        let y0 = (height.saturating_sub(bar_h)) / 2;

        // The track, always full width, so the indicator has a visible extent
        // before anything is typed — an empty screen gives no sign the machine
        // is even awake.
        fill(canvas, width, x0, y0, bar_w, bar_h, pack(cusk::theme::INSET));

        // Capped, and deliberately not proportional to the real length: a bar
        // that reaches the end at exactly your password's length publishes its
        // length to anyone watching. It saturates well before most passwords do.
        let filled = if self.state == State::Wrong {
            bar_w
        } else {
            let steps = self.password.chars().count().min(12) as u32;
            bar_w * steps / 12
        };
        if filled > 0 {
            fill(canvas, width, x0, y0, filled, bar_h, pack(self.state.colour()));
        }

        let buffer = pool.create_buffer(
            0,
            width as i32,
            height as i32,
            width as i32 * 4,
            wl_shm::Format::Argb8888,
            (),
            &self.qh,
        );
        surface.wl_surface().attach(Some(&buffer), 0, 0);
        surface.wl_surface().damage_buffer(0, 0, width as i32, height as i32);
        surface.wl_surface().commit();
        buffer.destroy();
    }

    fn size_of(&self, surface: &SessionLockSurface) -> Option<(u32, u32)> {
        self.sizes
            .iter()
            .find(|(s, _)| s == surface.wl_surface())
            .map(|(_, size)| *size)
    }
}

/// Pack a theme colour into the ARGB8888 a Wayland shm buffer wants.
fn pack(colour: cusk::theme::Rgba) -> u32 {
    let to_byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u32;
    (0xFF << 24) | (to_byte(colour[0]) << 16) | (to_byte(colour[1]) << 8) | to_byte(colour[2])
}

/// Fill an axis-aligned rectangle. Clipped, because a configure can arrive
/// between computing a rectangle and drawing it.
fn fill(canvas: &mut [u8], stride_px: u32, x: u32, y: u32, w: u32, h: u32, colour: u32) {
    let bytes = colour.to_le_bytes();
    for row in y..y.saturating_add(h) {
        for column in x..x.saturating_add(w) {
            let index = ((row as usize) * (stride_px as usize) + column as usize) * 4;
            if index + 4 <= canvas.len() {
                canvas[index..index + 4].copy_from_slice(&bytes);
            }
        }
    }
}

impl SessionLockHandler for Lock {
    fn locked(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _lock: SessionLock) {
        eprintln!("cusk-lock: session locked");
    }

    fn finished(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _lock: SessionLock) {
        // The compositor refused. Nothing is locked, so saying so and leaving is
        // the whole correct response.
        eprintln!("cusk-lock: the compositor refused to lock the session");
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _serial: u32,
    ) {
        let wl = surface.wl_surface().clone();
        match self.sizes.iter_mut().find(|(s, _)| *s == wl) {
            Some((_, size)) => *size = configure.new_size,
            None => self.sizes.push((wl, configure.new_size)),
        }
        self.draw(&surface);
    }
}

impl KeyboardHandler for Lock {
    fn enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _kb: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
        _raw: &[u32],
        _keysyms: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _kb: &wl_keyboard::WlKeyboard,
        _surface: &wl_surface::WlSurface,
        _serial: u32,
    ) {
        // Focus lost while locked should not happen — the compositor gives the
        // keyboard to the lock surface and nothing else can take it — but if it
        // does, the safe reading is that what was typed is no longer trusted.
        self.clear();
        self.state = State::Idle;
        self.redraw();
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _kb: &wl_keyboard::WlKeyboard,
        _serial: u32,
        event: KeyEvent,
    ) {
        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => self.attempt(),
            Keysym::Escape => {
                self.clear();
                self.state = State::Idle;
                self.redraw();
            }
            Keysym::BackSpace => {
                self.password.pop();
                self.state =
                    if self.password.is_empty() { State::Idle } else { State::Typing };
                self.redraw();
            }
            _ => {
                // `utf8` is what xkb produced for this key with the current
                // layout and modifiers — the right source for a password, and
                // the reason this uses SCTK's keyboard rather than raw keycodes.
                let Some(text) = event.utf8.as_deref() else { return };
                // Control characters are not password material; Ctrl+C arrives
                // here as U+0003 and would silently become part of it.
                if text.is_empty() || text.chars().any(|c| c.is_control()) {
                    return;
                }
                self.password.push_str(text);
                self.state = State::Typing;
                self.redraw();
            }
        }
    }

    fn release_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _kb: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _kb: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _modifiers: Modifiers,
        _raw_modifiers: RawModifiers,
        _layout: u32,
    ) {
    }

    /// Never reached: the keyboard is taken with repeat disabled.
    ///
    /// Implemented anyway rather than left to `todo!()`, because a repeat that
    /// somehow arrived would then panic the locker — and a panicking locker
    /// leaves the compositor locked with no client, which the protocol requires
    /// it to honour. The safe response to an unexpected repeat is to ignore it.
    fn repeat_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _kb: &wl_keyboard::WlKeyboard,
        _serial: u32,
        _event: KeyEvent,
    ) {
    }
}

impl SeatHandler for Lock {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            // Without repeat: a held key repeating into a password field turns
            // a leaned-on keyboard into a hundred characters, and there is
            // nothing here that wants a repeated keystroke.
            if let Ok(keyboard) = self.seat_state.get_keyboard(qh, &seat, None) {
                self.keyboard = Some(keyboard);
            }
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard {
            if let Some(keyboard) = self.keyboard.take() {
                keyboard.release();
            }
        }
    }

    fn remove_seat(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _seat: wl_seat::WlSeat) {}
}

impl CompositorHandler for Lock {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }
    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }
    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
    }
    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Lock {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }
    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }
}

impl ShmHandler for Lock {
    fn shm_state(&mut self) -> &mut Shm {
        &mut self.shm
    }
}

impl ProvidesRegistryState for Lock {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Lock);
delegate_output!(Lock);
delegate_shm!(Lock);
delegate_seat!(Lock);
delegate_keyboard!(Lock);
delegate_session_lock!(Lock);
delegate_registry!(Lock);

// `RawPool::create_buffer` hands back a `wl_buffer` this program never needs
// events from: it is destroyed immediately after the attach, and release is
// irrelevant because every frame allocates a fresh pool.
wayland_client::delegate_noop!(Lock: ignore wayland_client::protocol::wl_buffer::WlBuffer);
