//! Cusk — milestone 1.
//!
//! A Wayland compositor nested as a window inside the running session. It
//! accepts clients, maps their toplevels into a `Space`, positions and focuses
//! them, renders, and exits cleanly.
//!
//! No tiling, no config, no shell. The point is to exercise the whole spine —
//! socket, display, seat, output, xdg-shell, rendering, input routing — on real
//! hardware with a real client, before any of the interesting design gets
//! built on top of assumptions.
//!
//! Windows live in `smithay::desktop::Space` rather than a bare list of
//! toplevels. The difference matters: `Space` owns positions, and positions are
//! what both layout modes in `docs/cusk.md` §3 compute. A list would need
//! replacing the moment tiling arrives; this does not.
//!
//!     cargo run                       # spawns $TERMINAL, or the first found
//!     cargo run -- alacritty          # spawn something specific
//!     cargo run -- --no-spawn         # bring your own client
//!
//! Running it inside an existing session is deliberate. A compositor that has
//! never run is not a compositor that works, and finding that out from a TTY
//! costs a reboot.

use std::sync::Arc;

use smithay::backend::input::{InputEvent, KeyboardKeyEvent};
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::renderer::gles::GlesRenderer;
use smithay::backend::renderer::utils::{draw_render_elements, on_commit_buffer_handler};
use smithay::backend::renderer::{Color32F, Frame, Renderer};
use smithay::backend::winit::{self, WinitEvent};
use smithay::desktop::{Space, Window};
use smithay::input::keyboard::FilterResult;
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::protocol::wl_buffer;
use smithay::reexports::wayland_server::protocol::wl_seat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket};
use smithay::utils::{Rectangle, Serial, Transform};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    with_surface_tree_downward, CompositorClientState, CompositorHandler, CompositorState,
    SurfaceAttributes, TraversalAction,
};
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_seat, delegate_shm, delegate_xdg_shell,
};
// `::winit` — smithay re-exports a module of the same name, which shadows it.
use ::winit::platform::pump_events::PumpStatus;

/// Terminals tried in order when none is named. Every one of these is a
/// well-behaved Wayland client, which matters for a first run: an XWayland-only
/// client would fail for reasons that have nothing to do with the compositor.
const TERMINALS: &[&str] = &["foot", "alacritty", "kitty", "weston-terminal", "konsole"];

struct Cusk {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    shm_state: ShmState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    seat: Seat<Self>,
    /// Owns window positions. The substrate both layout modes will compute into.
    space: Space<Window>,
}

// ── protocol handlers ────────────────────────────────────────────────────

impl BufferHandler for Cusk {
    fn buffer_destroyed(&mut self, _buffer: &wl_buffer::WlBuffer) {}
}

impl CompositorHandler for Cusk {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor_state
    }

    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor_state
    }

    fn commit(&mut self, surface: &WlSurface) {
        // Turns the client's attached buffer into something renderable. Without
        // this the window exists, is focusable, and draws nothing — which looks
        // like a rendering bug and is not one.
        on_commit_buffer_handler::<Self>(surface);

        // A toplevel must be configured before it will attach a buffer. Missing
        // this deadlocks the client politely: it waits forever for a configure
        // that never comes, and the compositor looks idle rather than wrong.
        let mapped = self
            .space
            .elements()
            .find(|w| w.toplevel().is_some_and(|t| t.wl_surface() == surface))
            .cloned();
        if let Some(window) = mapped {
            if let Some(toplevel) = window.toplevel() {
                toplevel.send_configure();
            }
        }
        self.space.refresh();
    }
}

impl XdgShellHandler for Cusk {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell_state
    }

    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let window = Window::new_wayland_window(surface);
        // Cascade rather than stack at the origin, so a second window is
        // visibly a second window. Floating placement policy in miniature —
        // §3's floating mode is this, with intent.
        let n = self.space.elements().count() as i32;
        let location = (40 + n * 30, 40 + n * 30);
        self.space.map_element(window.clone(), location, true);

        if let Some(toplevel) = window.toplevel() {
            toplevel.send_configure();
        }
        tracing::info!("mapped toplevel at {location:?}");
        self.focus(&window);
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}

    fn grab(&mut self, _surface: PopupSurface, _seat: wl_seat::WlSeat, _serial: Serial) {}

    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let gone = self
            .space
            .elements()
            .find(|w| w.toplevel().map(|t| t.wl_surface()) == Some(surface.wl_surface()))
            .cloned();
        if let Some(window) = gone {
            self.space.unmap_elem(&window);
            tracing::info!("toplevel destroyed");
        }
        // Focus does not survive its window. Leaving a dead surface focused
        // sends keystrokes nowhere and looks like the keyboard has died.
        let next = self.space.elements().next_back().cloned();
        match next {
            Some(w) => self.focus(&w),
            None => {
                if let Some(kb) = self.seat.get_keyboard() {
                    kb.set_focus(self, None, Serial::from(0));
                }
            }
        }
    }
}

impl SeatHandler for Cusk {
    type KeyboardFocus = WlSurface;
    type PointerFocus = WlSurface;
    type TouchFocus = WlSurface;

    fn seat_state(&mut self) -> &mut SeatState<Self> {
        &mut self.seat_state
    }
    fn cursor_image(
        &mut self,
        _seat: &Seat<Self>,
        _image: smithay::input::pointer::CursorImageStatus,
    ) {
    }
    fn focus_changed(&mut self, _seat: &Seat<Self>, _focused: Option<&WlSurface>) {}
}

impl SelectionHandler for Cusk {
    type SelectionUserData = ();
}
impl DataDeviceHandler for Cusk {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device_state
    }
}
impl ClientDndGrabHandler for Cusk {}
impl ServerDndGrabHandler for Cusk {}

impl ShmHandler for Cusk {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl Cusk {
    /// Raise and give keyboard focus in one step.
    ///
    /// Kept together deliberately: a window that is focused but not raised, or
    /// raised but not focused, is the classic window-manager bug where typing
    /// goes to something you cannot see.
    fn focus(&mut self, window: &Window) {
        let location = self.space.element_location(window).unwrap_or_default();
        self.space.map_element(window.clone(), location, true);
        if let Some(kb) = self.seat.get_keyboard() {
            let surface = window.toplevel().map(|t| t.wl_surface().clone());
            kb.set_focus(self, surface, Serial::from(0));
        }
    }
}

delegate_compositor!(Cusk);
delegate_xdg_shell!(Cusk);
delegate_shm!(Cusk);
delegate_seat!(Cusk);
delegate_data_device!(Cusk);

#[derive(Default)]
struct ClientState {
    compositor_state: CompositorClientState,
}
impl ClientData for ClientState {
    fn initialized(&self, _id: ClientId) {
        tracing::debug!("client connected");
    }
    fn disconnected(&self, _id: ClientId, _reason: DisconnectReason) {
        tracing::debug!("client disconnected");
    }
}

/// Frame callbacks. A client that does not get these draws once and then waits
/// forever, which reads as "the app froze" rather than "the compositor never
/// asked for another frame".
fn send_frames(surface: &WlSurface, time: u32) {
    with_surface_tree_downward(
        surface,
        (),
        |_, _, &()| TraversalAction::DoChildren(()),
        |_, states, &()| {
            for callback in states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .frame_callbacks
                .drain(..)
            {
                callback.done(time);
            }
        },
        |_, _, &()| true,
    );
}

fn pick_terminal() -> Option<&'static str> {
    TERMINALS.iter().copied().find(|t| {
        std::process::Command::new("sh")
            .args(["-c", &format!("command -v {t} >/dev/null")])
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
    })
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cusk=info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let no_spawn = args.iter().any(|a| a == "--no-spawn");
    let requested: Option<String> =
        args.iter().find(|a| !a.starts_with('-')).cloned();

    let mut display: Display<Cusk> = Display::new()?;
    let mut dh = display.handle();

    let compositor_state = CompositorState::new::<Cusk>(&dh);
    let shm_state = ShmState::new::<Cusk>(&dh, vec![]);
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "cusk");

    let mut state = Cusk {
        compositor_state,
        xdg_shell_state: XdgShellState::new::<Cusk>(&dh),
        shm_state,
        data_device_state: DataDeviceState::new::<Cusk>(&dh),
        seat_state,
        seat,
        space: Space::default(),
    };

    // Let the socket be allocated rather than hardcoded. A fixed name collides
    // with any other nested compositor and, worse, can bind over a name a real
    // session is using.
    let listener = ListeningSocket::bind_auto("cusk", 1..32)?;
    let socket_name = listener
        .socket_name()
        .ok_or("socket has no name")?
        .to_string_lossy()
        .into_owned();
    tracing::info!("listening on {socket_name}");

    let keyboard = state.seat.add_keyboard(Default::default(), 200, 25)?;

    let (mut backend, mut winit_loop) = winit::init::<GlesRenderer>()?;
    let start = std::time::Instant::now();

    if !no_spawn {
        // Owned rather than borrowed. `pick_terminal` yields &'static str and
        // `requested` a local String; unifying the two by reference makes the
        // local's lifetime the binding constraint for no gain.
        let chosen: Option<String> =
            requested.clone().or_else(|| pick_terminal().map(str::to_owned));
        match chosen.as_deref() {
            Some(term) => {
                tracing::info!("spawning {term}");
                // Only the child's environment is changed. Setting
                // WAYLAND_DISPLAY on the compositor's own process would make it
                // a client of itself the next time anything connected.
                match std::process::Command::new(term)
                    .env("WAYLAND_DISPLAY", &socket_name)
                    .spawn()
                {
                    Ok(_) => {}
                    Err(e) => tracing::error!("could not spawn {term}: {e}"),
                }
            }
            None => tracing::warn!(
                "no terminal found (tried {TERMINALS:?}) — start one by hand with \
                 WAYLAND_DISPLAY={socket_name}"
            ),
        }
    } else {
        tracing::info!("--no-spawn: connect with WAYLAND_DISPLAY={socket_name}");
    }

    let mut clients = Vec::new();

    loop {
        let status = winit_loop.dispatch_new_events(|event| {
            if let WinitEvent::Input(InputEvent::Keyboard { event }) = event {
                keyboard.input::<(), _>(
                    &mut state,
                    event.key_code(),
                    event.state(),
                    Serial::from(0),
                    0,
                    |_, _, _| FilterResult::Forward,
                );
            }
        });

        if let PumpStatus::Exit(_) = status {
            tracing::info!("window closed, exiting");
            return Ok(());
        }

        let size = backend.window_size();
        let damage = Rectangle::from_size(size);
        {
            let (renderer, mut framebuffer) = backend.bind()?;

            // Positions come from the Space, not from the surface list. This is
            // the line that makes tiling a change of policy rather than a
            // rewrite.
            let elements: Vec<WaylandSurfaceRenderElement<GlesRenderer>> = state
                .space
                .elements()
                .filter_map(|window| {
                    let loc = state.space.element_location(window)?;
                    let surface = window.toplevel()?.wl_surface().clone();
                    Some(render_elements_from_surface_tree(
                        renderer,
                        &surface,
                        (loc.x, loc.y),
                        1.0,
                        1.0,
                        Kind::Unspecified,
                    ))
                })
                .flatten()
                .collect();

            let mut frame = renderer.render(&mut framebuffer, size, Transform::Flipped180)?;
            frame.clear(Color32F::new(0.05, 0.06, 0.09, 1.0), &[damage])?;
            draw_render_elements(&mut frame, 1.0, &elements, &[damage])?;
            let _sync = frame.finish()?;

            let now = start.elapsed().as_millis() as u32;
            for window in state.space.elements() {
                if let Some(toplevel) = window.toplevel() {
                    send_frames(toplevel.wl_surface(), now);
                }
            }

            if let Some(stream) = listener.accept()? {
                let client = dh.insert_client(stream, Arc::new(ClientState::default()))?;
                clients.push(client);
            }

            display.dispatch_clients(&mut state)?;
            display.flush_clients()?;
        }

        // After flushing: submit can block, and a client waiting on a message
        // still sitting in our buffer would be waiting on us.
        backend.submit(Some(&[damage]))?;
    }
}
