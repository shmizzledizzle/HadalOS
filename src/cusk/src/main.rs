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

use cusk::config;

mod chrome;
mod cursor;
mod floating;
mod geometry;
mod gpublur;
mod layout;
mod panel;
mod text;
mod tiling;
mod tty;
mod wallpaper;
mod workspace;

use std::sync::Arc;

use smithay::backend::input::{
    AbsolutePositionEvent, ButtonState, Event as _, InputEvent, KeyboardKeyEvent,
    PointerButtonEvent,
};
use smithay::backend::renderer::element::surface::{
    render_elements_from_surface_tree, WaylandSurfaceRenderElement,
};
use smithay::backend::renderer::element::Kind;
use smithay::backend::allocator::Fourcc;
use smithay::backend::renderer::gles::{GlesRenderer, GlesTexture};
use smithay::backend::renderer::utils::{draw_render_elements, on_commit_buffer_handler};
use smithay::backend::renderer::{Bind, Color32F, Frame, ImportMem, Renderer, RendererSuper};
use smithay::backend::winit::{self, WinitEvent};
use smithay::desktop::{Space, Window, WindowSurfaceType};
use smithay::backend::input::KeyState;
use smithay::input::keyboard::{FilterResult, Keysym, ModifiersState};
use smithay::reexports::wayland_protocols::xdg::decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode;
use smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel;

/// A compositor-level binding, returned from the keyboard filter so the event
/// loop can act on it after the borrow of the seat ends.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Binding {
    ToggleMaximize,
    ToggleTiling,
    ToggleFloating,
    CycleLayout,
    Widen(i32),
    Spawn,
    Launcher,
    FocusStep(isize),
    MoveInOrder(isize),
    Promote,
    Workspace(usize),
    SendToWorkspace(usize),
}
use smithay::input::pointer::{AxisFrame,ButtonEvent, GrabStartData, MotionEvent};
use smithay::input::{Seat, SeatHandler, SeatState};
use smithay::reexports::wayland_server::protocol::wl_buffer;
use smithay::reexports::wayland_server::protocol::wl_seat;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::reexports::wayland_server::backend::{ClientData, ClientId, DisconnectReason};
use smithay::reexports::wayland_server::{Client, Display, ListeningSocket};
use smithay::utils::{Logical, Physical, Point, Rectangle, Serial, Size, Transform, SERIAL_COUNTER};
use smithay::wayland::buffer::BufferHandler;
use smithay::wayland::compositor::{
    with_surface_tree_downward, CompositorClientState, CompositorHandler, CompositorState,
    SurfaceAttributes, TraversalAction,
};
use smithay::wayland::selection::data_device::{
    ClientDndGrabHandler, DataDeviceHandler, DataDeviceState, ServerDndGrabHandler,
};
use smithay::wayland::selection::SelectionHandler;
use smithay::wayland::shell::xdg::decoration::{XdgDecorationHandler, XdgDecorationState};
use smithay::wayland::shell::xdg::{
    PopupSurface, PositionerState, ToplevelSurface, XdgShellHandler, XdgShellState,
};
use smithay::wayland::dmabuf::{
    DmabufFeedbackBuilder, DmabufGlobal, DmabufHandler, DmabufState, ImportNotifier,
};
use smithay::wayland::shm::{ShmHandler, ShmState};
use smithay::{
    delegate_compositor, delegate_data_device, delegate_dmabuf, delegate_seat, delegate_shm,
    delegate_xdg_decoration, delegate_xdg_shell,
};
// `::winit` — smithay re-exports a module of the same name, which shadows it.
use ::winit::platform::pump_events::PumpStatus;

/// Terminals tried in order when none is named. Every one of these is a
/// well-behaved Wayland client, which matters for a first run: an XWayland-only
/// client would fail for reasons that have nothing to do with the compositor.
/// Which modifier arms the compositor's drag bindings.
///
/// Super is correct for a real session and wrong for a nested one: KDE's
/// default `CommandAllKey` is Meta, bound to Meta+LMB move and Meta+RMB
/// resize — the same gestures — so KWin consumes them before the nested window
/// sees anything. `CUSK_MOD=alt` exists so the bindings can be exercised under
/// a host that has already claimed Super.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ModKey {
    Super,
    Alt,
    Ctrl,
    CtrlAlt,
}

impl ModKey {
    /// Resolve from the config, with `CUSK_MOD` overriding it.
    ///
    /// The env var stays because it is a testing affordance for nested runs
    /// under a host that claims the same modifier, and editing a config file
    /// to try the other one is friction in exactly the wrong place.
    fn resolve(configured: &str) -> Self {
        let chosen = std::env::var("CUSK_MOD").unwrap_or_else(|_| configured.to_string());
        Self::parse(&chosen)
    }

    fn parse(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "alt" => ModKey::Alt,
            "ctrl" => ModKey::Ctrl,
            "ctrl-alt" | "ctrlalt" => ModKey::CtrlAlt,
            "" | "super" | "logo" | "meta" => ModKey::Super,
            other => {
                tracing::warn!("mod key {other:?} not recognised, using super");
                ModKey::Super
            }
        }
    }

    fn held(self, m: &ModifiersState) -> bool {
        match self {
            ModKey::Super => m.logo,
            ModKey::Alt => m.alt,
            ModKey::Ctrl => m.ctrl,
            ModKey::CtrlAlt => m.ctrl && m.alt,
        }
    }

    fn label(self) -> &'static str {
        match self {
            ModKey::Super => "super",
            ModKey::Alt => "alt",
            ModKey::Ctrl => "ctrl",
            ModKey::CtrlAlt => "ctrl + alt",
        }
    }
}

/// The app id cusk treats as an overlay: exempt from tiling and centred.
/// Must match `cusk-launcher`'s own constant; changing one without the other
/// turns the launcher back into an ordinary tiled window.
const OVERLAY_APP_ID: &str = "cusk-launcher";

/// What `classify` has worked out about a window so far.
///
/// Two separate questions with two separate arrival times, which is why this
/// is not one boolean: *what* a window is comes from `set_app_id`, and *how
/// big* it is only exists once it has committed a buffer. Placing it on the
/// first commit centres a 0x0 window, which puts its top-left corner in the
/// middle of the screen — the failure that produced `x: 640` on a 1280-wide
/// output.
#[derive(Default)]
struct Classification {
    /// `None` until an app id has been seen at all.
    overlay: std::cell::Cell<Option<bool>>,
    placed: std::cell::Cell<bool>,
}

struct Cusk {
    compositor_state: CompositorState,
    xdg_shell_state: XdgShellState,
    /// Held, not read: the global lives as long as this does, and dropping it
    /// would withdraw `zxdg_decoration_manager_v1` from clients that have
    /// already bound it.
    #[allow(dead_code)]
    xdg_decoration_state: XdgDecorationState,
    shm_state: ShmState,
    dmabuf_state: DmabufState,
    seat_state: SeatState<Self>,
    data_device_state: DataDeviceState,
    seat: Seat<Self>,
    /// Owns window positions. The substrate both layout modes will compute into.
    space: Space<Window>,
    /// Pointer position in compositor-global coordinates. Kept here rather than
    /// read back from the seat because grabs need the value from before the
    /// grab started, and the seat only knows 'now'.
    pointer_location: Point<f64, smithay::utils::Logical>,
    /// Live modifier state, for compositor-level bindings like Super+drag.
    modifiers: ModifiersState,
    /// Which modifier arms those bindings.
    mod_key: ModKey,

    /// Whether hovering a window focuses it.
    focus_follows_mouse: bool,
    /// Height of the workspace bar. Zero disables it entirely.
    panel_height: i32,
    /// Dmabufs a client has offered but that have not been tested against the
    /// renderer yet. Drained every frame, where the renderer is reachable.
    pending_dmabufs: Vec<(
        smithay::backend::allocator::dmabuf::Dmabuf,
        smithay::wayland::dmabuf::ImportNotifier,
    )>,
    /// What the pointer should look like right now, as clients request it.
    cursor: smithay::input::pointer::CursorImageStatus,
    /// Every workspace, and which one is on screen.
    ///
    /// Order, tiling mode, layout and focus all live per-workspace: switching
    /// to a tiled workspace and back must not leave the other one tiled.
    workspaces: workspace::Workspaces<Window>,
    gaps: layout::Gaps,
    /// Current output size in logical coordinates, kept so relayout does not
    /// need the render loop to hand it over.
    output_size: (i32, i32),
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
            // Recomputes the window's cached bounding box from the surface
            // tree. Without it the bbox stays 0x0 for the window's whole life,
            // and every hit test fails: `Space::element_under` asks the bbox,
            // so no click ever lands, nothing focuses, and a client's own
            // decorations never receive the press that would send
            // `xdg_toplevel.move`.
            //
            // Rendering hides this completely. `render_elements_from_surface_tree`
            // walks the surface tree directly and never consults the cached
            // geometry, so the window draws perfectly while being, as far as
            // input is concerned, zero pixels wide.
            window.on_commit();
            self.classify(&window);

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
        // `app_id` is deliberately *not* read here. `xdg_toplevel.set_app_id`
        // is a separate request that arrives after the toplevel is created, so
        // at this point it is always `None` — a window is classified on its
        // first commit instead, in `classify`.
        let window = Window::new_wayland_window(surface);
        // Cascade rather than stack at the origin, so a second window is
        // visibly a second window. Floating placement policy in miniature —
        // §3's floating mode is this, with intent.
        let n = self.space.elements().count() as i32;
        let usable = panel::usable_area(
            Size::from((self.output_size.0, self.output_size.1)),
            self.panel_height,
        );
        let location = (40 + n * 30, usable.loc.y + 40 + n * 30);
        self.space.map_element(window.clone(), location, true);

        if let Some(toplevel) = window.toplevel() {
            toplevel.send_configure();
        }
        tracing::info!("mapped toplevel at {location:?}");
        // Give the window a floating rectangle from birth, so maximise-first
        // has somewhere to restore to instead of silently doing nothing.
        geometry::remember(&self.space, &window);

        // A toplevel with a parent is a dialog. §3's floating exception: tiling
        // one is always wrong, so it is applied from the protocol rather than
        // waiting for the user to notice a file chooser has become a tile.
        let is_dialog = window
            .toplevel()
            .and_then(|t| t.parent())
            .is_some();
        if is_dialog {
            geometry::set_exempt(&window, true);
            tracing::info!("dialog exempted from tiling");
        }

        self.workspaces.insert(window.clone());
        self.focus(&window);
        self.relayout();
    }

    /// A client asking to be dragged — a CSD titlebar. Honoured with the same
    /// grab a Super+drag uses, so both paths behave identically.
    fn move_request(&mut self, surface: ToplevelSurface, _seat: wl_seat::WlSeat, _serial: Serial) {
        let found = self
            .space
            .elements()
            .find(|w| w.toplevel().map(|t| t.wl_surface()) == Some(surface.wl_surface()))
            .cloned();
        // Info, like `start_move`. Without it, "the titlebar does not drag"
        // cannot be told apart from "the client never asked" — and those have
        // completely different causes.
        tracing::info!("client requested a move");
        if let Some(window) = found {
            self.start_move(window, floating::BTN_LEFT);
        }
    }

    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: wl_seat::WlSeat,
        _serial: Serial,
        edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
        let found = self
            .space
            .elements()
            .find(|w| w.toplevel().map(|t| t.wl_surface()) == Some(surface.wl_surface()))
            .cloned();
        if let Some(window) = found {
            self.start_resize(window, floating::BTN_LEFT, edges);
        }
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
            // Drop it from the tile order too, or the layout keeps reserving a
            // column for a window that no longer exists — a gap that looks
            // like a rendering fault rather than stale bookkeeping.
            self.workspaces.remove(&window);
            tracing::info!("toplevel destroyed");
        }
        self.relayout();
        // Focus does not survive its window. Leaving a dead surface focused
        // sends keystrokes nowhere and looks like the keyboard has died.
        let next = self.space.elements().next_back().cloned();
        match next {
            Some(w) => self.focus(&w),
            None => {
                self.workspaces.active_mut().focused = None;
                if let Some(kb) = self.seat.get_keyboard() {
                    kb.set_focus(self, None, Serial::from(0));
                }
            }
        }
    }
}

/// Who draws a window's titlebar.
///
/// §3 says floating and tiling are two policies over one window set, and this
/// is where that becomes visible. In **floating** mode a window is its own
/// object: it gets its titlebar, which is what a client offers to drag. In
/// **tiling** mode position is computed and the bar is a lie — it cannot be
/// dragged anywhere meaningful, it eats a row of every tile, and the thing it
/// names is already in the panel.
///
/// A client that insists on drawing its own is not overruled. The protocol is
/// a negotiation and some toolkits cannot turn their decorations off; forcing
/// the mode would leave a window that renders wrongly rather than one with a
/// spare titlebar.
impl XdgDecorationHandler for Cusk {
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.tell_decoration(&toplevel);
    }

    fn request_mode(&mut self, toplevel: ToplevelSurface, _mode: DecorationMode) {
        // The client's preference is heard and then answered with the
        // compositor's, which is what the protocol expects: the client asks,
        // the compositor decides.
        self.tell_decoration(&toplevel);
    }

    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.tell_decoration(&toplevel);
    }
}
delegate_xdg_decoration!(Cusk);

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
        image: smithay::input::pointer::CursorImageStatus,
    ) {
        // Recorded, not ignored. A client that asks for an I-beam over its text
        // and gets an arrow is a small wrongness; a compositor that draws no
        // pointer at all is an unusable one, and this handler being empty was
        // the reason for the second.
        self.cursor = image;
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

impl DmabufHandler for Cusk {
    fn dmabuf_state(&mut self) -> &mut DmabufState {
        &mut self.dmabuf_state
    }

    fn dmabuf_imported(
        &mut self,
        _global: &DmabufGlobal,
        dmabuf: smithay::backend::allocator::dmabuf::Dmabuf,
        notifier: ImportNotifier,
    ) {
        // The renderer lives in the winit backend, which the event loop owns,
        // so the import cannot happen here. Queued instead, and answered on the
        // next frame — the notifier is what tells the client whether its buffer
        // was accepted, and dropping one without answering leaves the client
        // waiting forever for a reply that never comes.
        self.pending_dmabufs.push((dmabuf, notifier));
    }
}
delegate_dmabuf!(Cusk);

impl ShmHandler for Cusk {
    fn shm_state(&self) -> &ShmState {
        &self.shm_state
    }
}

impl Cusk {

    /// Window and surface under a compositor-global point, with the surface's
    /// origin — the three things pointer routing always needs together.
    fn surface_under(
        &self,
        point: Point<f64, smithay::utils::Logical>,
    ) -> Option<(Window, WlSurface, Point<f64, smithay::utils::Logical>)> {
        let (window, window_loc) = self.space.element_under(point)?;
        let window = window.clone();

        // Descend to the actual surface, not the toplevel's root.
        //
        // A client's decorations are subsurfaces. Focusing the root means the
        // titlebar never receives a click, so it never sends
        // `xdg_toplevel.move`, so dragging it does nothing — and the window
        // still looks focused, because the root surface is getting the events.
        // Popups are included for the same reason: a menu that cannot be
        // clicked is worse than one that never opened.
        let (surface, surface_loc) =
            window.surface_under(point - window_loc.to_f64(), WindowSurfaceType::ALL)?;

        // Surface positions are relative to the window; the pointer needs them
        // global, or every client computes its local coordinates from the wrong
        // origin and hit-testing is subtly off everywhere.
        Some((window, surface, (surface_loc + window_loc).to_f64()))
    }

    /// Synthesise grab start data for a compositor-initiated drag.
    ///
    /// Client-initiated grabs arrive with a serial the client already had;
    /// these do not, so one is minted. Without a serial the grab is silently
    /// declined and Super+drag does nothing at all.
    fn grab_start(&self, button: u32) -> GrabStartData<Cusk> {
        let focus = self
            .surface_under(self.pointer_location)
            .map(|(_, surface, loc)| (surface, loc));
        GrabStartData { focus, button, location: self.pointer_location }
    }

    fn start_move(&mut self, window: Window, button: u32) {
        let Some(pointer) = self.seat.get_pointer() else { return };
        let initial = self.space.element_location(&window).unwrap_or_default();
        // Info, not debug. Whether a gesture was *recognised* is the first
        // question when nothing visibly happens, and requiring RUST_LOG to
        // answer it means the answer arrives one run too late.
        // The gesture is the same; what it means is not. A tiled window has no
        // position of its own to change, so dragging it reorders instead.
        if self.is_tiled(&window) {
            tracing::info!("swap grab started");
            let grab = tiling::SwapGrab {
                start_data: self.grab_start(button),
                window,
            };
            pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), smithay::input::pointer::Focus::Clear);
            return;
        }

        tracing::info!("move grab started at {initial:?}");
        let grab = floating::MoveGrab {
            start_data: self.grab_start(button),
            window,
            initial_window_location: initial,
        };
        pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), smithay::input::pointer::Focus::Clear);
    }

    fn start_resize(
        &mut self,
        window: Window,
        button: u32,
        edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
        let Some(pointer) = self.seat.get_pointer() else { return };

        // A tile cannot be resized alone — its neighbours must yield the space
        // — so the gesture drags the divider rather than one window's edge.
        if self.is_tiled(&window) {
            tracing::info!("ratio grab started");
            let grab = tiling::RatioGrab { start_data: self.grab_start(button) };
            pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), smithay::input::pointer::Focus::Clear);
            return;
        }

        let rect = floating::window_rect(&self.space, &window);
        tracing::info!("resize grab started, edge {edges:?}, from {:?}", rect.size);
        if let Some(toplevel) = window.toplevel() {
            toplevel.with_pending_state(|state| {
                state.states.set(
                    smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::State::Resizing,
                );
            });
            toplevel.send_pending_configure();
        }
        let grab = floating::ResizeGrab {
            start_data: self.grab_start(button),
            window,
            edges,
            initial_rect: rect,
            last_requested: rect.size,
        };
        pointer.set_grab(self, grab, SERIAL_COUNTER.next_serial(), smithay::input::pointer::Focus::Clear);
    }

    /// Move keyboard focus to the next or previous window.
    ///
    /// Cycles `order`, not stacking order. Stacking order is a most-recently-
    /// used list, so cycling it walks back and forth between the same two
    /// windows instead of touring them all.
    fn focus_step(&mut self, delta: isize) {
        let windows = self.order().clone();
        if windows.is_empty() {
            return;
        }
        let focused = self.focused();
        let current = focused
            .as_ref()
            .and_then(|f| windows.iter().position(|w| w == f))
            .unwrap_or(0);
        let next = layout::step(windows.len(), current, delta);
        self.focus(&windows[next]);
        tracing::info!("focus {} of {}", next + 1, windows.len());
    }

    /// Move the focused window earlier or later in the tile order.
    fn move_in_order(&mut self, delta: isize) {
        let Some(focused) = self.focused() else { return };
        let Some(from) = self.order().iter().position(|w| w == &focused) else { return };
        if self.order().len() < 2 {
            return;
        }
        let to = layout::step(self.order().len(), from, delta);
        self.order_mut().swap(from, to);
        tracing::info!("moved window from {from} to {to}");
        self.relayout();
    }

    /// Make the focused window the master.
    ///
    /// Swap rather than remove-and-insert: promoting sends the old master to
    /// where the promoted window was, so pressing it twice returns to the
    /// arrangement you started from. Shifting everything down instead makes
    /// the gesture irreversible, and there is no undo in a window manager.
    fn promote(&mut self) {
        let Some(focused) = self.focused() else { return };
        let Some(from) = self.order().iter().position(|w| w == &focused) else { return };
        if from == 0 {
            return;
        }
        self.order_mut().swap(0, from);
        tracing::info!("promoted window {from} to master");
        self.relayout();
    }

    pub fn order(&self) -> &Vec<Window> {
        &self.workspaces.active().order
    }

    pub fn order_mut(&mut self) -> &mut Vec<Window> {
        &mut self.workspaces.active_mut().order
    }

    pub fn tiling(&self) -> bool {
        self.workspaces.active().tiling
    }

    pub fn layout(&self) -> layout::Layout {
        self.workspaces.active().layout
    }

    pub fn focused(&self) -> Option<Window> {
        self.workspaces.active().focused.clone()
    }

    /// Decide what a window *is*, once, on its first commit.
    ///
    /// Not at `new_toplevel`: `set_app_id` is a separate request that has not
    /// arrived yet, so the id is always `None` there. Reading it too early is
    /// silent — the window simply never matches, and the special case looks
    /// like it was never written.
    fn classify(&mut self, window: &Window) {
        window.user_data().insert_if_missing(Classification::default);
        let (known, placed) = {
            let data = window
                .user_data()
                .get::<Classification>()
                .expect("just inserted");

            if data.overlay.get().is_none() {
                let app_id = window.toplevel().and_then(|toplevel| {
                    smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
                        states
                            .data_map
                            .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                            .and_then(|d| d.lock().ok().and_then(|d| d.app_id.clone()))
                    })
                });
                // Recorded only once an id has actually arrived. Deciding "not
                // an overlay" from a `None` that simply has not been sent yet
                // is the bug this replaced.
                if let Some(app_id) = app_id {
                    data.overlay.set(Some(app_id == OVERLAY_APP_ID));
                }
            }
            (data.overlay.get(), data.placed.get())
        };

        if known != Some(true) || placed {
            return;
        }

        // Exempt as soon as it is known to be an overlay, before it is placed:
        // a relayout in between would otherwise tile it for a frame.
        geometry::set_exempt(window, true);

        // Placement waits for a real size.
        let size = window.geometry().size;
        if size.w <= 0 || size.h <= 0 {
            return;
        }
        if let Some(data) = window.user_data().get::<Classification>() {
            data.placed.set(true);
        }

        // Centred horizontally, high vertically — a launcher pinned to the
        // exact middle sits under the pointer and covers what you were looking
        // at.
        let location = Point::from((
            ((self.output_size.0 - size.w) / 2).max(0),
            ((self.output_size.1 - size.h) / 3).max(0),
        ));
        self.space.map_element(window.clone(), location, true);
        geometry::remember(&self.space, window);
        self.focus(window);
        self.relayout();
        tracing::info!("overlay {OVERLAY_APP_ID} centred at {location:?} ({}x{})", size.w, size.h);
    }

    /// The focused window's title, as the client last set it.
    fn focused_title(&self) -> Option<String> {
        let window = self.focused()?;
        let toplevel = window.toplevel()?;
        smithay::wayland::compositor::with_states(toplevel.wl_surface(), |states| {
            states
                .data_map
                .get::<smithay::wayland::shell::xdg::XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok().and_then(|data| data.title.clone()))
        })
        .filter(|title| !title.trim().is_empty())
    }

    /// A click on the panel, if it landed on one.
    ///
    /// Returns whether the click was the panel's, so the caller knows not to
    /// forward it. A press that both switches workspace *and* reaches whatever
    /// is underneath would activate something on the workspace being left.
    fn panel_click(&mut self, at: Point<i32, Logical>) -> bool {
        let output = Size::from((self.output_size.0, self.output_size.1));
        if !panel::contains(output, self.panel_height, at) {
            return false;
        }
        let pills = panel::pills(
            output,
            self.panel_height,
            self.workspaces.len(),
            self.workspaces.active_index(),
        );
        if let Some(index) = panel::pill_at(&pills, at) {
            self.switch_workspace(index);
        }
        // Consumed either way. The bar is the compositor's strip of screen, so
        // a click on an empty part of it belongs to nothing rather than
        // falling through to a window that happens to be behind it.
        true
    }

    /// Tell one window who draws its decorations.
    fn tell_decoration(&mut self, toplevel: &ToplevelSurface) {
        let tiling = self.tiling();
        toplevel.with_pending_state(|state| {
            state.decoration_mode = Some(if tiling {
                // Server side, and cusk draws nothing — which is how the bar
                // disappears. The window's identity lives in the panel.
                DecorationMode::ServerSide
            } else {
                DecorationMode::ClientSide
            });
        });
        toplevel.send_pending_configure();
    }

    /// Tell every window, after the mode changes.
    fn tell_all_decorations(&mut self) {
        let windows: Vec<Window> = self.space.elements().cloned().collect();
        for window in windows {
            if let Some(toplevel) = window.toplevel() {
                self.tell_decoration(&toplevel.clone());
            }
        }
    }

    /// Show a different workspace.
    fn switch_workspace(&mut self, index: usize) {
        let Some(switch) = self.workspaces.switch_to(index) else { return };

        // Unmapped, not moved off-screen. A window parked at a huge coordinate
        // is still in the Space: it takes part in hit testing, in layout and in
        // "topmost window" queries, so the compositor keeps acting on windows
        // nobody can see.
        for window in &switch.hide {
            self.space.unmap_elem(window);
        }
        for window in &switch.show {
            // Remembered geometry travels with the window, so a floating
            // window returns to where it was rather than to the origin. The
            // fallback only applies to a window that never had a rectangle.
            let location = geometry::recall(window)
                .map(|rect| rect.loc)
                .unwrap_or_else(|| Point::from((40, 40)));
            self.space.map_element(window.clone(), location, false);
        }

        match switch.focus {
            Some(window) => self.focus(&window),
            None => {
                // An empty workspace must actually drop focus, or keystrokes
                // keep going to a window on a workspace that is no longer
                // shown.
                if let Some(kb) = self.seat.get_keyboard() {
                    kb.set_focus(self, None, Serial::from(0));
                }
            }
        }
        self.relayout();
        let occupied: Vec<String> = self
            .workspaces
            .occupied()
            .iter()
            .enumerate()
            .filter(|(_, has)| **has)
            .map(|(i, _)| (i + 1).to_string())
            .collect();
        // Until there is a panel, this line is the only workspace indicator
        // cusk has — and switching to an empty workspace looks identical to
        // the compositor having hung.
        tracing::info!(
            "workspace {} of {} (windows on: {})",
            self.workspaces.active_index() + 1,
            self.workspaces.len(),
            if occupied.is_empty() { "none".into() } else { occupied.join(", ") }
        );
    }

    /// Send the focused window to another workspace.
    fn send_to_workspace(&mut self, index: usize) {
        let Some(window) = self.focused() else { return };
        let Some(focus) = self.workspaces.move_to(&window, index) else { return };

        self.space.unmap_elem(&window);
        if let Some(next) = focus {
            self.focus(&next);
        } else if let Some(kb) = self.seat.get_keyboard() {
            kb.set_focus(self, None, Serial::from(0));
        }
        self.relayout();
        tracing::info!("sent window to workspace {}", index + 1);
    }

    /// Whether the layout currently owns this window's geometry.
    ///
    /// Both halves matter: tiling can be off, and an individual window can be
    /// exempt from it while it is on.
    fn is_tiled(&self, window: &Window) -> bool {
        self.tiling() && !geometry::is_exempt(window)
    }

    /// The windows the layout is entitled to place, in stable order.
    pub fn tiled(&self) -> Vec<Window> {
        self.order()
            .iter()
            .filter(|w| !geometry::is_exempt(w))
            .cloned()
            .collect()
    }

    /// Compute and apply the current policy over the current window set.
    ///
    /// Safe to call after any change to either. Doing the work unconditionally
    /// rather than tracking dirtiness is deliberate at this size: a missed
    /// invalidation shows up as a window that silently stops participating in
    /// the layout, which is far harder to see than a redundant recompute.
    pub fn relayout(&mut self) {
        if !self.tiling() {
            return;
        }
        let windows = self.tiled();
        // The one place the usable area is computed, so tiling, placement and
        // maximise cannot disagree about where the screen starts.
        let area = panel::usable_area(
            Size::from((self.output_size.0, self.output_size.1)),
            self.panel_height,
        );
        let tiles = self.layout().arrange(area, windows.len(), self.gaps);

        for (window, tile) in windows.iter().zip(tiles) {
            // Record the floating rectangle before displacing, so leaving
            // tiling can put the window back. `remember` is a no-op once the
            // window is already displaced, so this does not overwrite the
            // rectangle on every subsequent relayout.
            geometry::remember(&self.space, window);
            geometry::set_displaced(window, true);

            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.size = Some(tile.size);
                    // Tells the client it is tiled so it can drop rounded
                    // corners and drop shadows, which otherwise leave visible
                    // seams between tiles that look like gap bugs.
                    state.states.set(xdg_toplevel::State::TiledLeft);
                    state.states.set(xdg_toplevel::State::TiledRight);
                    state.states.set(xdg_toplevel::State::TiledTop);
                    state.states.set(xdg_toplevel::State::TiledBottom);
                });
                toplevel.send_pending_configure();
            }
            self.space.map_element(window.clone(), tile.loc, false);
        }
    }

    /// Switch the workspace between tiled and floating.
    fn toggle_tiling(&mut self) {
        let now = !self.tiling();
        self.workspaces.active_mut().tiling = now;
        if now {
            self.relayout();
        } else {
            // §3: "Switching a workspace from tiled to floating — tiled
            // windows need remembered floating geometry, or they all pile at
            // the origin." This is that restore, and the reason the geometry
            // module exists.
            for window in self.tiled() {
                geometry::set_displaced(&window, false);
                let Some(rect) = geometry::recall(&window) else { continue };
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| {
                        state.size = Some(rect.size);
                        state.states.unset(xdg_toplevel::State::TiledLeft);
                        state.states.unset(xdg_toplevel::State::TiledRight);
                        state.states.unset(xdg_toplevel::State::TiledTop);
                        state.states.unset(xdg_toplevel::State::TiledBottom);
                    });
                    toplevel.send_pending_configure();
                }
                self.space.map_element(window.clone(), rect.loc, false);
            }
        }
        // The decoration mode is part of the mode switch, not a side effect of
        // it: leaving the bars behind when tiling turns on is exactly the
        // half-applied state this call prevents.
        self.tell_all_decorations();
        tracing::info!(
            "tiling {} ({})",
            if self.tiling() { "on" } else { "off" },
            self.layout().name()
        );
    }

    /// Exempt the focused window from tiling, or return it to the layout.
    fn toggle_floating(&mut self, window: &Window) {
        let now = !geometry::is_exempt(window);
        geometry::set_exempt(window, now);
        if now {
            // Leaving the layout means going back to where it floated.
            geometry::set_displaced(window, false);
            if let Some(rect) = geometry::recall(window) {
                if let Some(toplevel) = window.toplevel() {
                    toplevel.with_pending_state(|state| state.size = Some(rect.size));
                    toplevel.send_pending_configure();
                }
                self.space.map_element(window.clone(), rect.loc, true);
            }
        }
        tracing::info!("window {} the layout", if now { "left" } else { "rejoined" });
        self.relayout();
    }

    /// Fill the output, or return to the remembered floating rectangle.
    ///
    /// §3 calls maximise "neither mode": it is a departure from floating that
    /// must be undoable, which is exactly what remembered geometry is for. It
    /// is also the cheapest way to prove the remembering works, since the
    /// window has to come back to the pixel.
    fn toggle_maximize(&mut self, window: &Window, output_size: (i32, i32)) {
        // A tiled window is already displaced, so without this guard the
        // toggle takes the restore branch and pops the window back to its
        // floating rectangle while tiling is still on — leaving one window
        // loose over a layout that still believes it owns that tile.
        if self.is_tiled(window) {
            tracing::info!("window is tiled; maximise does not apply");
            return;
        }
        if geometry::is_displaced(window) {
            let Some(rect) = geometry::recall(window) else { return };
            geometry::set_displaced(window, false);
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.size = Some(rect.size);
                    state.states.unset(xdg_toplevel::State::Maximized);
                });
                toplevel.send_pending_configure();
            }
            self.space.map_element(window.clone(), rect.loc, true);
            tracing::info!("restored to {:?}", rect);
        } else {
            // Record before displacing, not after: once the flag is set the
            // recording is frozen, so the order here is the difference between
            // remembering the floating rectangle and remembering nothing.
            geometry::remember(&self.space, window);
            if geometry::recall(window).is_none() {
                // Nothing to come back to; displacing now would strand the
                // window maximised with the toggle unable to undo it.
                tracing::warn!("no floating geometry yet, not maximising");
                return;
            }
            geometry::set_displaced(window, true);
            let area = panel::usable_area(
                Size::from((output_size.0, output_size.1)),
                self.panel_height,
            );
            if let Some(toplevel) = window.toplevel() {
                toplevel.with_pending_state(|state| {
                    state.size = Some(area.size);
                    state.states.set(xdg_toplevel::State::Maximized);
                });
                toplevel.send_pending_configure();
            }
            self.space.map_element(window.clone(), area.loc, true);
            tracing::info!("maximised, will restore to {:?}", geometry::recall(window));
        }
    }

    /// Apply a reloaded configuration to the running compositor.
    ///
    /// Only the settings the schema marks `Apply::Live`. The rest describe
    /// initial state, and reapplying them would overrule choices the user has
    /// made since — reloading the file must not undo a layout picked with
    /// Super+E.
    fn apply_config(&mut self, cfg: &config::Config) {
        // Safe to call whenever: shrinking rehomes windows onto the last
        // surviving workspace rather than dropping them, because losing a
        // window because a number in a config file got smaller would be
        // unrecoverable from inside the session.
        if cfg.workspace_count.max(1) as usize != self.workspaces.len() {
            self.workspaces
                .resize(cfg.workspace_count.max(1) as usize, cfg.tiling_on_start, self.layout());
            // The rehomed windows belong to whatever workspace is now active;
            // anything mapped that no longer does has to go.
            let visible = self.workspaces.active().order.clone();
            let mapped: Vec<Window> = self.space.elements().cloned().collect();
            for window in mapped {
                if !visible.contains(&window) {
                    self.space.unmap_elem(&window);
                }
            }
            for window in &visible {
                if self.space.element_location(window).is_none() {
                    let at = geometry::recall(window)
                        .map(|r| r.loc)
                        .unwrap_or_else(|| Point::from((40, 40)));
                    self.space.map_element(window.clone(), at, false);
                }
            }
            tracing::info!("now {} workspaces", self.workspaces.len());
        }
        self.gaps = layout::Gaps { inner: cfg.inner_gap, outer: cfg.outer_gap };
        self.focus_follows_mouse = cfg.focus_follows_mouse;
        self.panel_height = cfg.panel_height;
        self.mod_key = ModKey::resolve(&cfg.mod_key);
        // The ratio lives inside the master-stack variant, so it can only be
        // applied while that is the layout. Columns has no divider to move.
        if let layout::Layout::MasterStack { .. } = self.layout() {
            self.workspaces.active_mut().layout = layout::Layout::MasterStack { ratio: cfg.master_ratio };
        }
        self.relayout();
    }

    /// Focus whatever the pointer is over, if it is not already focused.
    ///
    /// Two guards, both of which are the difference between this being usable
    /// and being unbearable:
    ///
    /// - **Nothing under the pointer changes nothing.** Crossing a gap between
    ///   tiles would otherwise drop focus, so typing would stop mid-sentence
    ///   whenever the pointer sat in a gap.
    /// - **Already-focused windows are skipped.** `focus` raises, and raising
    ///   on every motion event re-stacks the space hundreds of times a second.
    fn focus_under_pointer(&mut self, location: Point<f64, smithay::utils::Logical>) {
        let Some((window, _, _)) = self.surface_under(location) else { return };
        if self.focused().as_ref() == Some(&window) {
            return;
        }
        self.focus(&window);
    }

    /// Raise and give keyboard focus in one step.
    ///
    /// Kept together deliberately: a window that is focused but not raised, or
    /// raised but not focused, is the classic window-manager bug where typing
    /// goes to something you cannot see.
    fn focus(&mut self, window: &Window) {
        self.workspaces.active_mut().focused = Some(window.clone());
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
/// Everything the backdrop depends on. When this changes, the textures are
/// rebuilt; while it does not, they are reused.
#[derive(Debug, Clone, PartialEq, Eq)]
struct BackdropKey {
    path: String,
    size: (i32, i32),
    radius: i32,
    passes: i32,
}

/// The wallpaper, uploaded: sharp for the desktop, blurred for behind windows.
struct Backdrop {
    /// What produced the sharp texture. Decoding and scaling is the expensive
    /// half, so it is keyed separately from the blur — changing the blur
    /// radius must not re-read the file.
    source: (String, (i32, i32)),
    blur: (i32, i32),
    /// Kept so the blur can be recomputed without touching the disk.
    scaled: wallpaper::Image,
    sharp: GlesTexture,
    blurred: GlesTexture,
}

fn upload(renderer: &mut GlesRenderer, image: &wallpaper::Image) -> Option<GlesTexture> {
    renderer
        .import_memory(
            &image.data,
            // ABGR8888 is little-endian A:B:G:R, which in memory is the byte
            // order R,G,B,A that the decoder produces. Naming it the other way
            // round swaps red and blue, and the mistake looks like an oddly
            // tinted wallpaper rather than a format bug.
            Fourcc::Abgr8888,
            Size::from((image.width as i32, image.height as i32)),
            false,
        )
        .ok()
}

impl Backdrop {
    fn build(renderer: &mut GlesRenderer, key: &BackdropKey) -> Option<Self> {
        let started = std::time::Instant::now();
        let scaled = match wallpaper::load_scaled(
            std::path::Path::new(&key.path),
            (key.size.0.max(1) as u32, key.size.1.max(1) as u32),
        ) {
            Ok(image) => image,
            // Reported once per change, not once per frame: the key is stored
            // by the caller either way, so a broken path complains and then
            // stays quiet.
            Err(e) => {
                tracing::warn!("wallpaper {}: {e}", key.path);
                return None;
            }
        };
        let sharp = upload(renderer, &scaled)?;
        let blurred_image =
            wallpaper::blurred_from(&scaled, key.radius.max(0) as u32, key.passes.max(1) as u32);
        let blurred = upload(renderer, &blurred_image)?;

        tracing::info!(
            "wallpaper ready in {}ms ({}x{}, blur r{} x{})",
            started.elapsed().as_millis(),
            key.size.0,
            key.size.1,
            key.radius,
            key.passes
        );
        Some(Backdrop {
            source: (key.path.clone(), key.size),
            blur: (key.radius, key.passes),
            scaled,
            sharp,
            blurred,
        })
    }

    /// Recompute only the blur, reusing the decoded and scaled image.
    fn reblur(&mut self, renderer: &mut GlesRenderer, key: &BackdropKey) {
        let started = std::time::Instant::now();
        let image =
            wallpaper::blurred_from(&self.scaled, key.radius.max(0) as u32, key.passes.max(1) as u32);
        if let Some(texture) = upload(renderer, &image) {
            self.blurred = texture;
            self.blur = (key.radius, key.passes);
            tracing::info!(
                "reblurred in {}ms (r{} x{})",
                started.elapsed().as_millis(),
                key.radius,
                key.passes
            );
        }
    }
}

/// The backdrop the current configuration and output size call for, or `None`
/// when no wallpaper is set.
fn backdrop_key(cfg: &config::Config, size: Size<i32, Logical>) -> Option<BackdropKey> {
    let path = cfg.wallpaper.trim();
    if path.is_empty() {
        return None;
    }
    Some(BackdropKey {
        path: path.to_string(),
        size: (size.w, size.h),
        radius: cfg.blur_radius,
        passes: cfg.blur_passes,
    })
}

/// A logical rectangle in physical pixels.
///
/// Written out rather than assumed. cusk runs at scale 1, where the numbers
/// are identical and the conversion looks pointless — which is exactly why it
/// is spelled out: it stops being identity on the first HiDPI output, and a
/// missing conversion there is invisible until it is everywhere.
fn to_physical(rect: Rectangle<i32, Logical>) -> Rectangle<i32, Physical> {
    Rectangle::new(
        Point::from((rect.loc.x, rect.loc.y)),
        Size::from((rect.size.w, rect.size.h)),
    )
}

/// A logical rectangle as a texture source crop.
///
/// Only correct because both backdrop textures are built at exactly the output
/// size — see `wallpaper::prepare`, which does that so this conversion can be
/// the identity rather than a scale.
fn texture_src(rect: Rectangle<i32, Logical>) -> Rectangle<f64, smithay::utils::Buffer> {
    Rectangle::new(
        Point::from((rect.loc.x as f64, rect.loc.y as f64)),
        Size::from((rect.size.w as f64, rect.size.h as f64)),
    )
}

/// Run cusk on a virtual terminal.
///
/// The winit loop's counterpart: obtain a framebuffer, pump input, call
/// `draw_frame`, present, dispatch clients. Everything about *what* is drawn is
/// shared — that was the point of milestones 21 and 24.
///
/// Time-boxed for now. VT switching and session pause/resume are not handled,
/// so a compositor that ran indefinitely would keep DRM master across a switch
/// away and leave the other VT blank. Escape ends it early; the watchdog ends
/// it regardless.
fn run_on_tty(
    mut cfg: config::Config,
    seconds: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut session, notifier) = smithay::backend::session::libseat::LibSeatSession::new()
        .map_err(|e| format!("could not join a session: {e}\n  This needs its own VT."))?;
    let seat_name = smithay::backend::session::Session::seat(&session);

    // Opened before the display is taken, so anything that fails here fails
    // while the console is still readable.
    let mut drm = tty::Drm::open(&mut session)?;
    let mut libinput = tty::libinput_for(&session, &seat_name)?;

    let Compositor {
        mut display,
        mut state,
        listener,
        socket_name,
        keyboard,
        pointer,
    } = build_compositor(&cfg).map_err(|e| {
        // Overwhelmingly this is `XDG_RUNTIME_DIR` missing, and overwhelmingly
        // that means someone reached for `sudo` — which strips it. Saying so
        // here saves the guess, because the message from the socket layer
        // names the variable without naming the cause.
        format!(
            "{e}\n  If this says XDG_RUNTIME_DIR: run cusk as yourself, not under sudo.\n  \
             The tty backend does not need root."
        )
    })?;
    let mut dh = display.handle();

    // The one global the driver registers itself, because it needs the
    // renderer's formats and only a driver has a renderer.
    let _dmabuf_global = if drm.formats.is_empty() {
        tracing::warn!("no dmabuf render formats; clients will use shared memory");
        None
    } else {
        let count = drm.formats.len();
        let global = state
            .dmabuf_state
            .create_global::<Cusk>(&dh, drm.formats.clone());
        tracing::info!("dmabuf advertised with {count} formats");
        Some(global)
    };

    state.output_size = drm.size;
    println!();
    println!("  cusk on {seat_name}, {}x{}", drm.size.0, drm.size.1);
    println!("  WAYLAND_DISPLAY={socket_name}");
    println!("  escape to quit    ctrl+alt+F1..F12 to switch terminal");
    if seconds > 0 {
        println!("  {seconds}s limit");
    }
    println!();

    // Watching before the display is taken, so a switch during startup is not
    // missed and cusk does not draw over a VT it no longer owns.
    let (mut session_events, mut active) = tty::watch_session(notifier)?;

    // Armed before the display is taken, so a hang anywhere after this point
    // still ends with a usable console. `seconds == 0` means no limit, which
    // is only safe now that a VT switch releases the display — before
    // pause/resume worked, an unbounded run could hold the screen forever.
    if seconds > 0 {
        drm.arm_watchdog(seconds + 3);
    } else {
        println!("  no time limit — escape is the way out");
    }
    drm.take_display();

    let mut ctx = FrameContext {
        chrome: None,
        blur: None,
        backdrop: None,
        refused: None,
        face: text::find_font(&cfg.font).and_then(|path| text::Face::load(&path)),
        title_texture: None,
        pointer_image: cursor::arrow(24),
        pointer_texture: None,
        warned_square_corners: false,
    };
    // Resolved once, as the winit driver does, so Super+Return has something
    // to spawn. Mutable because a config reload can change it.
    let mut terminal: Option<String> = match cfg.terminal.as_str() {
        "auto" => pick_terminal().map(str::to_owned),
        named => Some(named.to_string()),
    };
    // Hot reload works on the tty too. It was winit-only, which meant the
    // settings editor could be open on one backend and inert on the other —
    // the same parity gap as the bindings and the drag, in the one feature
    // whose whole point is that an edit lands immediately.
    let mut watcher = config::Watcher::new(config::default_path());

    let mut clients = Vec::new();
    let mut chord = tty::Chord::default();
    let start = std::time::Instant::now();
    let deadline = (seconds > 0).then(|| start + std::time::Duration::from_secs(seconds));

    let outcome = (|| -> Result<(), Box<dyn std::error::Error>> {
        while deadline.is_none_or(|deadline| std::time::Instant::now() < deadline) {
            // Zero timeout: this is a poll inside the render loop, not the
            // loop itself.
            session_events.dispatch(Some(std::time::Duration::ZERO), &mut active)?;

            if !active.active {
                // Nothing is drawn and no input is read while another VT has
                // the display. Drawing would write to a revoked fd and fail
                // every frame; reading input would steal keys from whoever is
                // actually using the machine.
                libinput.suspend();
                std::thread::sleep(std::time::Duration::from_millis(50));
                display.flush_clients()?;
                continue;
            }

            if std::mem::take(&mut active.just_resumed) {
                // Devices were revoked and handed back, so libinput needs to
                // reopen them. The mode needs no special handling: `present`
                // sets the CRTC every frame, so the first frame after a
                // resume restores it.
                if libinput.resume().is_err() {
                    tracing::warn!("libinput could not resume; input may be dead");
                }
            }

            let input = tty::drain(&mut libinput);
            if input.escape {
                break;
            }

            let time = start.elapsed().as_millis() as u32;
            for (code, pressed) in input.keys {
                // Checked before the key reaches the compositor. Holding
                // session control means logind has disabled the kernel's own
                // Ctrl+Alt+F<n>, so a compositor that does not implement it
                // traps the user on its VT — which is what happened on the
                // first unbounded run.
                if let Some(vt) = chord.key(code, pressed) {
                    tracing::info!("switching to VT {vt}");
                    if let Err(e) = smithay::backend::session::Session::change_vt(&mut session, vt)
                    {
                        tracing::warn!("could not switch to VT {vt}: {e}");
                    }
                    // Not forwarded. A client receiving the F-key as well would
                    // act on it while the screen is being handed away.
                    continue;
                }

                // The same table the winit driver uses, so a binding cannot
                // work on one backend and not the other — which is exactly
                // what shipped: a tty session with no way to open a terminal.
                let binding = keyboard.input::<Option<Binding>, _>(
                    &mut state,
                    // libinput reports evdev codes; xkb wants them offset by
                    // eight. Feeding the raw code shifts every key by one row,
                    // which reads as a broken layout rather than an offset.
                    smithay::backend::input::Keycode::from(code + 8),
                    if pressed {
                        smithay::backend::input::KeyState::Pressed
                    } else {
                        smithay::backend::input::KeyState::Released
                    },
                    SERIAL_COUNTER.next_serial(),
                    time,
                    |state, modifiers, handle| {
                        state.modifiers = *modifiers;
                        if pressed && state.mod_key.held(modifiers) {
                            if let Some(binding) =
                                binding_for(handle.modified_sym(), modifiers.shift)
                            {
                                // Intercepted, not forwarded, or the terminal
                                // receives a 'd' every time the launcher opens.
                                return FilterResult::Intercept(Some(binding));
                            }
                        }
                        FilterResult::Forward
                    },
                );
                if let Some(Some(binding)) = binding {
                    state.apply_binding(
                        binding,
                        &cfg,
                        terminal.as_deref(),
                        &socket_name,
                        drm.size,
                    );
                }
            }

            // Motion first, so a click in the same drain lands where the
            // pointer has just arrived rather than where it used to be.
            if input.motion != (0.0, 0.0) {
                let at = tty::clamp_pointer(
                    (state.pointer_location.x, state.pointer_location.y),
                    input.motion,
                    drm.size,
                );
                let location = Point::<f64, Logical>::from((at.0, at.1));
                state.pointer_location = location;

                // The same routing the winit path uses, so hover, focus and
                // grabs behave identically on both backends.
                let under = state
                    .surface_under(location)
                    .map(|(_, surface, loc)| (surface, loc));
                if state.focus_follows_mouse {
                    state.focus_under_pointer(location);
                }
                pointer.motion(
                    &mut state,
                    under,
                    &MotionEvent { location, serial: SERIAL_COUNTER.next_serial(), time },
                );
                pointer.frame(&mut state);
            }

            for scroll in input.scrolls {
                if scroll.is_empty() {
                    continue;
                }
                pointer.axis(&mut state, axis_frame(scroll, time));
                pointer.frame(&mut state);
            }

            for (button, pressed) in input.buttons {
                let serial = SERIAL_COUNTER.next_serial();
                // The same press handling the winit driver uses, so the panel,
                // click-to-focus and Super+drag behave identically. Without
                // this the tty session could focus a window and not move it.
                let forward = if pressed { state.press(button, serial) } else { true };
                if !forward {
                    continue;
                }
                pointer.button(
                    &mut state,
                    &ButtonEvent {
                        button,
                        state: if pressed {
                            ButtonState::Pressed
                        } else {
                            ButtonState::Released
                        },
                        serial,
                        time,
                    },
                );
                pointer.frame(&mut state);
            }

            let size = Size::<i32, Physical>::from((drm.size.0, drm.size.1));
            let logical = Size::<i32, Logical>::from((drm.size.0, drm.size.1));
            drm.with_back(|renderer, framebuffer| {
                draw_frame(
                    renderer,
                    framebuffer,
                    &mut state,
                    &mut ctx,
                    &cfg,
                    size,
                    logical,
                    // Not flipped: winit hands back an inverted framebuffer and
                    // DRM does not.
                    Transform::Normal,
                    start,
                )
            })??;

            // A revoked device is a VT switch seen before the notification,
            // not a fault. Treated as a pause here; the notifier confirms it a
            // moment later and `just_resumed` still fires on the way back.
            // Propagating it is what made cusk exit on the first switch.
            if let Err(e) = drm.present() {
                if tty::Drm::is_revoked(&e) {
                    tracing::debug!("display revoked mid-frame; pausing");
                    active.active = false;
                    continue;
                }
                return Err(e.into());
            }

            match watcher.poll() {
                config::Reload::Unchanged => {}
                config::Reload::Applied { config: fresh, complaints } => {
                    for complaint in &complaints {
                        tracing::warn!("{}: {}", watcher.path().display(), complaint);
                    }
                    for key in config::restart_only_changes(&cfg, &fresh) {
                        tracing::info!("{key} changed; takes effect on restart");
                    }
                    state.apply_config(&fresh);
                    terminal = match fresh.terminal.as_str() {
                        "auto" => pick_terminal().map(str::to_owned),
                        named => Some(named.to_string()),
                    };
                    cfg = fresh;
                }
                config::Reload::Failed(e) => {
                    tracing::warn!(
                        "{}: {e} — keeping the running configuration",
                        watcher.path().display()
                    );
                }
            }

            if let Some(stream) = listener.accept()? {
                clients.push(dh.insert_client(stream, Arc::new(ClientState::default()))?);
            }
            display.dispatch_clients(&mut state)?;
            display.flush_clients()?;
        }
        Ok(())
    })();

    drm.restore();
    outcome
}

/// Which compositor binding a keysym asks for, if any.
///
/// A pure function so both drivers agree by construction. The tty driver
/// shipped without any of these — a session with a wallpaper, a panel and no
/// way to open a terminal — because the whole table lived inside the winit
/// event handler.
///
/// The caller checks the modifier: whether the chord is armed depends on
/// `ModKey` and the seat's state, and neither belongs in a lookup table.
fn binding_for(sym: Keysym, shift: bool) -> Option<Binding> {
    match sym {
        Keysym::m => Some(Binding::ToggleMaximize),
        Keysym::t => Some(Binding::ToggleTiling),
        Keysym::space => Some(Binding::ToggleFloating),
        Keysym::e => Some(Binding::CycleLayout),
        Keysym::l => Some(Binding::Widen(1)),
        Keysym::h => Some(Binding::Widen(-1)),
        Keysym::Return | Keysym::KP_Enter => Some(Binding::Spawn),
        Keysym::d => Some(Binding::Launcher),
        Keysym::j => Some(Binding::FocusStep(1)),
        Keysym::k => Some(Binding::FocusStep(-1)),
        // Shift gives the capitalised keysym, so the
        // shifted bindings are distinguished here
        // rather than by re-reading modifier state.
        Keysym::J => Some(Binding::MoveInOrder(1)),
        Keysym::K => Some(Binding::MoveInOrder(-1)),
        Keysym::P => Some(Binding::Promote),
        // Digits pick a workspace; shifted digits send
        // the focused window to one. Shift produces a
        // different keysym per layout (! " # on some,
        // symbols on others), so the unshifted keysym
        // is read and the modifier checked separately —
        // matching on the shifted symbol works on one
        // keyboard layout and silently fails on the
        // rest.
        sym => match sym.raw() {
            0x0031..=0x0039 => {
                let index = (sym.raw() - 0x0031) as usize;
                Some(if shift {
                    Binding::SendToWorkspace(index)
                } else {
                    Binding::Workspace(index)
                })
            }
            _ => None,
        },
    }
}

/// Carry out a binding.
///
/// A method rather than a closure in the event handler, for the same reason
/// `draw_frame` is a function: two drivers, one behaviour. Spawning needs the
/// socket name and the configured programs, which is why they are arguments
/// rather than fields — they belong to the session, not to the compositor.
impl Cusk {
    fn apply_binding(
        &mut self,
        binding: Binding,
        cfg: &config::Config,
        terminal: Option<&str>,
        socket_name: &str,
        output_size: (i32, i32),
    ) {
        // Topmost in stacking order is the focused window.
        let focused = self.focused();
        match binding {
            Binding::ToggleMaximize => {
                if let Some(w) = focused {
                    self.toggle_maximize(&w, output_size);
                }
            }
            Binding::ToggleTiling => self.toggle_tiling(),
            Binding::ToggleFloating => {
                if let Some(w) = focused {
                    self.toggle_floating(&w);
                }
            }
            Binding::CycleLayout => {
                let next = self.layout().next();
                self.workspaces.active_mut().layout = next;
                tracing::info!("layout: {}", next.name());
                self.relayout();
            }
            Binding::FocusStep(d) => self.focus_step(d),
            Binding::MoveInOrder(d) => self.move_in_order(d),
            Binding::Promote => self.promote(),
            Binding::Workspace(i) => self.switch_workspace(i),
            Binding::SendToWorkspace(i) => self.send_to_workspace(i),
            Binding::Launcher => {
                let program = resolve_launcher(&cfg.launcher);
                match std::process::Command::new(&program)
                    .env("WAYLAND_DISPLAY", socket_name)
                    .spawn()
                {
                    Ok(child) => {
                        tracing::info!("launcher {program} (pid {})", child.id());
                        std::thread::spawn(move || {
                let mut child = child;
                let _ = child.wait();
                        });
                    }
                    Err(e) => tracing::warn!("could not run {program}: {e}"),
                }
            }
            Binding::Spawn => match terminal {
                Some(term) => spawn_terminal(term, socket_name),
                None => tracing::warn!("no terminal to spawn"),
            },
            Binding::Widen(dir) => {
                let wider = self.layout().widen(0.05 * dir as f64);
                self.workspaces.active_mut().layout = wider;
                self.relayout();
            }
        }
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;

    /// Every binding the banner advertises must actually resolve. A table this
    /// long is exactly where an entry goes missing, and the symptom is one
    /// dead key among a dozen working ones.
    #[test]
    fn the_advertised_bindings_all_resolve() {
        for (sym, expected) in [
            (Keysym::Return, Binding::Spawn),
            (Keysym::d, Binding::Launcher),
            (Keysym::t, Binding::ToggleTiling),
            (Keysym::e, Binding::CycleLayout),
            (Keysym::m, Binding::ToggleMaximize),
            (Keysym::space, Binding::ToggleFloating),
            (Keysym::j, Binding::FocusStep(1)),
            (Keysym::k, Binding::FocusStep(-1)),
            (Keysym::l, Binding::Widen(1)),
            (Keysym::h, Binding::Widen(-1)),
            (Keysym::J, Binding::MoveInOrder(1)),
            (Keysym::K, Binding::MoveInOrder(-1)),
            (Keysym::P, Binding::Promote),
        ] {
            assert_eq!(
                binding_for(sym, false),
                Some(expected),
                "{sym:?} resolves to nothing"
            );
        }
    }

    /// Digits pick a workspace; shifted digits send a window to one. Shift is
    /// read as modifier state rather than as a shifted keysym, because that
    /// symbol differs per keyboard layout.
    #[test]
    fn digits_pick_a_workspace_and_shift_sends_to_it() {
        assert_eq!(binding_for(Keysym::_1, false), Some(Binding::Workspace(0)));
        assert_eq!(binding_for(Keysym::_9, false), Some(Binding::Workspace(8)));
        assert_eq!(binding_for(Keysym::_1, true), Some(Binding::SendToWorkspace(0)));
        assert_eq!(binding_for(Keysym::_3, true), Some(Binding::SendToWorkspace(2)));
    }

    /// An unbound key must forward, or ordinary typing disappears whenever the
    /// modifier happens to be down.
    #[test]
    fn unbound_keys_are_not_claimed() {
        for sym in [Keysym::a, Keysym::z, Keysym::F5, Keysym::Escape, Keysym::_0] {
            assert_eq!(binding_for(sym, false), None, "{sym:?} was claimed");
        }
    }
}

impl Cusk {
    /// Handle a pointer press, returning whether the client should still see
    /// it.
    ///
    /// Lifted out of the winit handler for the third time in three milestones,
    /// and for the same reason: welded into one driver, it was invisible to
    /// the other. The tty session had click-to-focus and no Super+drag, so
    /// floating windows could be clicked and not moved.
    ///
    /// The order is the substance. The panel owns its strip outright and is
    /// tested first, or a floating window overlapping the bar takes the click.
    /// Focus is taken before any modifier handling, so a Super+drag also
    /// focuses. Consumed presses are not forwarded, or Super+drag selects text
    /// in the terminal it is dragging.
    fn press(&mut self, button: u32, serial: Serial) -> bool {
        if self.panel_click(self.pointer_location.to_i32_round()) {
            return false;
        }

        let Some((window, _, _)) = self.surface_under(self.pointer_location) else {
            // Clicking the background clears focus. Otherwise the last window
            // keeps the keyboard while looking inert.
            if let Some(keyboard) = self.seat.get_keyboard() {
                keyboard.set_focus(self, None, serial);
            }
            return true;
        };

        self.focus(&window);

        // Logged unconditionally at debug: when a host compositor eats the
        // modifier the symptom is that nothing happens, and nothing happening
        // is indistinguishable from a bug in the grab.
        tracing::debug!(
            "button {button:#x} mods: super={} alt={} ctrl={} shift={}",
            self.modifiers.logo,
            self.modifiers.alt,
            self.modifiers.ctrl,
            self.modifiers.shift,
        );

        if !self.mod_key.held(&self.modifiers) {
            return true;
        }
        match button {
            floating::BTN_LEFT => {
                self.start_move(window, button);
                false
            }
            floating::BTN_RIGHT => {
                let rect = floating::window_rect(&self.space, &window);
                let edges = floating::nearest_edge(rect, self.pointer_location);
                self.start_resize(window, button, edges);
                false
            }
            _ => true,
        }
    }
}

/// Turn a scroll into the frame a client is sent.
///
/// Shared by both drivers, because the mapping has enough judgement in it to
/// be worth having in one place: the source changes how a client interprets
/// the numbers, wheels carry discrete steps as well as smooth ones, and a
/// finger lift is a zero that has to be announced rather than dropped.
fn axis_frame(scroll: tty::Scroll, time: u32) -> AxisFrame {
    use smithay::backend::input::{Axis, AxisSource};

    let mut frame = AxisFrame::new(time).source(match scroll.source {
        tty::ScrollSource::Wheel => AxisSource::Wheel,
        tty::ScrollSource::Finger => AxisSource::Finger,
        tty::ScrollSource::Continuous => AxisSource::Continuous,
    });

    if let Some((h, v)) = scroll.v120 {
        if h != 0.0 {
            frame = frame.v120(Axis::Horizontal, h as i32);
        }
        if v != 0.0 {
            frame = frame.v120(Axis::Vertical, v as i32);
        }
    }
    if scroll.horizontal != 0.0 {
        frame = frame.value(Axis::Horizontal, scroll.horizontal);
    }
    if scroll.vertical != 0.0 {
        frame = frame.value(Axis::Vertical, scroll.vertical);
    }

    // A finger lift is reported as a zero on the axis that stopped, and a
    // client needs it to end kinetic scrolling. Dropped, a touchpad flick
    // keeps coasting, which reads as the scroll being stuck.
    let (stop_h, stop_v) = scroll.stopped();
    if stop_h {
        frame = frame.stop(Axis::Horizontal);
    }
    if stop_v {
        frame = frame.stop(Axis::Vertical);
    }
    frame
}

/// Everything the render loop needs that outlives a single frame.
///
/// Gathered into one place because the loop used to thread ten separate
/// locals through the render block, and a second driver would have to thread
/// the same ten. Shader programs, uploaded textures and the backdrop cache all
/// belong to the renderer's lifetime rather than to a frame.
struct FrameContext {
    chrome: Option<chrome::Chrome>,
    blur: Option<gpublur::GpuBlur>,
    backdrop: Option<Backdrop>,
    /// A key that failed to build, so it is not retried every frame.
    refused: Option<BackdropKey>,
    face: Option<text::Face>,
    title_texture: Option<(String, GlesTexture)>,
    pointer_image: cursor::Cursor,
    pointer_texture: Option<GlesTexture>,
    warned_square_corners: bool,
}

/// Draw one frame into `framebuffer`.
///
/// Split out of the loop so a second backend can call it. Everything here is
/// about *what* is on screen; obtaining a framebuffer, presenting it and
/// pumping input are the driver's job, and those are the only parts that
/// differ between running nested and running on a tty.
///
/// `transform` is the driver's, not the compositor's: winit hands back a
/// framebuffer that is already flipped, and DRM does not.
#[allow(clippy::too_many_arguments)]
fn draw_frame(
    renderer: &mut GlesRenderer,
    framebuffer: &mut <GlesRenderer as RendererSuper>::Framebuffer<'_>,
    state: &mut Cusk,
    ctx: &mut FrameContext,
    current: &config::Config,
    size: Size<i32, Physical>,
    logical_size: Size<i32, Logical>,
    transform: Transform,
    start: std::time::Instant,
) -> Result<(), Box<dyn std::error::Error>> {
    let damage = Rectangle::from_size(size);
        // Compiled on the first frame rather than at startup, because it
        // needs a current GL context and the renderer only has one here.
        let chrome = ctx.chrome.get_or_insert_with(|| chrome::Chrome::new(renderer));
        let blur = match &mut ctx.blur {
            Some(blur) => blur,
            slot => slot.insert(gpublur::GpuBlur::new(renderer)),
        };

        // Rebuild the backdrop only when something it depends on changes.
        // Preparing costs a decode, two resizes and six blur passes, which
        // is fine once and unacceptable per frame.
        // Two levels, because decoding and scaling cost an order of
        // magnitude more than blurring. Dragging the blur radius must not
        // re-read the file.
        match backdrop_key(&current, logical_size) {
            None => ctx.backdrop = None,
            // A key that has already failed is not retried. The comment
            // here used to claim a failure was "reported once per change,
            // because the key is stored either way" — it was not: a failed
            // build leaves `backdrop` as `None`, so the next frame tried
            // again, and a missing wallpaper produced 1020 identical
            // warnings in a seventeen-second run.
            Some(key) if ctx.refused.as_ref() == Some(&key) => {}
            Some(key) => match &mut ctx.backdrop {
                Some(existing) if existing.source == (key.path.clone(), key.size) => {
                    if existing.blur != (key.radius, key.passes) {
                        existing.reblur(renderer, &key);
                    }
                }
                _ => {
                    ctx.backdrop = Backdrop::build(renderer, &key);
                    ctx.refused = ctx.backdrop.is_none().then_some(key);
                }
            },
        }

        // Built before the frame, because constructing elements and
        // uploading textures both need the renderer mutably and the frame
        // borrows it for its whole life.
        //
        // Grouped per window rather than flattened, so each window's blur
        // patch can be drawn immediately beneath it. A single flat list
        // would force every patch to be drawn before every window, and the
        // patch of an upper window would then sit on top of a lower one.
        let mut layers: Vec<(
            Rectangle<i32, Logical>,
            bool,
            Vec<WaylandSurfaceRenderElement<GlesRenderer>>,
        )> = Vec::new();
        for window in state.space.elements() {
            let Some(loc) = state.space.element_location(window) else { continue };
            let Some(surface) = window.toplevel().map(|t| t.wl_surface().clone()) else {
                continue;
            };
            // The fifth argument is alpha, and it has been 1.0 since
            // milestone 1. Setting it here rather than drawing the window
            // through a shader is what keeps subsurfaces and popups
            // working: they are separate elements in this tree, and each
            // one carries the same alpha.
            //
            // It also switches off occlusion culling for the window, which
            // is required rather than incidental: `opaque_regions` returns
            // empty below 1.0, so whatever is behind a translucent window
            // still gets drawn instead of being skipped as hidden.
            let elements = render_elements_from_surface_tree(
                renderer,
                &surface,
                (loc.x, loc.y),
                1.0,
                current.window_opacity as f32,
                Kind::Unspecified,
            );
            let focused = state.focused().as_ref() == Some(window);
            layers.push((Rectangle::new(loc, window.geometry().size), focused, elements));
        }

        // Answered here because this is where the renderer is reachable.
        // A notifier dropped without a verdict leaves the client waiting
        // for a reply that never comes — it does not fall back, it hangs,
        // which looks like the client froze rather than like the compositor
        // failed to answer.
        for (dmabuf, notifier) in state.pending_dmabufs.drain(..) {
            use smithay::backend::renderer::ImportDma;
            match renderer.import_dmabuf(&dmabuf, None) {
                Ok(_) => {
                    let _ = notifier.successful::<Cusk>();
                }
                Err(e) => {
                    tracing::warn!("rejected a dmabuf: {e}");
                    notifier.failed();
                }
            }
        }

        // Built before the frame for the same reason the window layers are:
        // constructing elements needs the renderer mutably, and the frame
        // borrows it for its whole life.
        let cursor_elements: Option<Vec<WaylandSurfaceRenderElement<GlesRenderer>>> =
            match &state.cursor {
                smithay::input::pointer::CursorImageStatus::Surface(surface) => {
                    // The hotspot comes from the surface's own role data.
                    // Assuming (0,0) puts a text I-beam's tip at its
                    // top-left corner, so text lands a glyph off from where
                    // it was aimed.
                    let hotspot =
                        smithay::wayland::compositor::with_states(surface, |states| {
                            states
                                .data_map
                                .get::<smithay::input::pointer::CursorImageSurfaceData>()
                                .and_then(|d| d.lock().ok().map(|d| d.hotspot))
                                .unwrap_or_default()
                        });
                    let at = state.pointer_location.to_i32_round() - hotspot;
                    Some(render_elements_from_surface_tree(
                        renderer,
                        surface,
                        (at.x, at.y),
                        1.0,
                        1.0,
                        Kind::Cursor,
                    ))
                }
                _ => None,
            };

        // The focused window's title, prepared here because rasterising
        // and uploading both need the renderer and the frame borrows it.
        let title: Option<(Rectangle<i32, Logical>, (u32, u32), GlesTexture)> = (|| {
            if state.panel_height <= 0 {
                return None;
            }
            let face = ctx.face.as_mut()?;
            let output = Size::from((logical_size.w, logical_size.h));
            let pills_end = panel::pills(
                output,
                state.panel_height,
                state.workspaces.len(),
                state.workspaces.active_index(),
            )
            .last()
            .map(|p| p.loc.x + p.size.w)
            .unwrap_or(0);

            let size = (state.panel_height as f32 * 0.5).clamp(9.0, 20.0);
            // Kept clear of the pills on *both* sides, so a centred title
            // cannot slide under them on a narrow screen.
            let budget = logical_size.w - (pills_end + 16) * 2;
            let text = face.truncate(&state.focused_title()?, size, budget);
            if text.is_empty() {
                return None;
            }
            let width = face.measure(&text, size);
            let image = face.render(&text, size, cusk::theme::TEXT)?;
            let dimensions = (image.width, image.height);

            // Re-uploaded only when the string changes. A title is drawn
            // every frame and changes rarely; uploading per frame would be
            // the launcher icon's mistake a third time.
            let stale = ctx.title_texture
                .as_ref()
                .map(|(cached, _)| cached != &text)
                .unwrap_or(true);
            if stale {
                ctx.title_texture = upload(renderer, image).map(|t| (text.clone(), t));
            }
            let (_, texture) = ctx.title_texture.as_ref()?;
            let rect = Rectangle::<i32, Logical>::new(
                Point::from((
                    (logical_size.w - width) / 2,
                    (state.panel_height - dimensions.1 as i32) / 2,
                )),
                Size::from((width, dimensions.1 as i32)),
            );
            Some((rect, dimensions, texture.clone()))
        })();

        // Uploaded once. The arrow never changes, so rebuilding it per
        // frame would be a texture upload per frame for a 24x24 image.
        if ctx.pointer_texture.is_none() {
            ctx.pointer_texture = upload(renderer, &ctx.pointer_image.image);
        }

        // Window blur assembles the scene in an offscreen texture so each
        // window can blur what is behind it *before* it is drawn. Off by
        // default: it is a blur chain per window per frame, where the
        // wallpaper blur it replaces costs nothing.
        let live_blur = current.window_blur
            && ctx.backdrop.is_some()
            && blur.begin(renderer, (size.w, size.h));

        if live_blur {
            // Held in an `Option` and handed back and forth with `blur`,
            // because blurring needs the texture inside the struct while
            // drawing needs it outside. Anything else is two mutable
            // borrows of one struct.
            let mut held = blur.take_scene();
            if let Some(scene) = held.as_mut() {
                // The wallpaper, into the scene rather than the screen.
                {
                    let mut target = renderer.bind(scene)?;
                    let mut f = renderer.render(&mut target, size, Transform::Normal)?;
                    f.clear(Color32F::new(0.03, 0.07, 0.10, 1.0), &[damage])?;
                    if let Some(backdrop) = &ctx.backdrop {
                        let whole = Rectangle::from_size(logical_size);
                        Frame::render_texture_from_to(
                            &mut f,
                            &backdrop.sharp,
                            texture_src(whole),
                            Rectangle::from_size(size),
                            &[damage],
                            &[],
                            Transform::Normal,
                            1.0,
                        )?;
                    }
                    let _ = f.finish();
                }

                for (rect, focused, elements) in &layers {
                    let dst = to_physical(*rect);

                    // Blur the scene as it stands — which is everything
                    // behind this window and nothing in front, because the
                    // scene is being built back to front.
                    if let Some(scene) = held.take() {
                        blur.put_scene(scene);
                    }
                    let has_blur = blur
                        .blur_scene(renderer, current.blur_radius, current.blur_passes as u32)
                        .is_some();
                    held = blur.take_scene();
                    let Some(scene) = held.as_mut() else { break };

                    if has_blur {
                        if let Some(blurred) = blur.blurred() {
                            let mut target = renderer.bind(scene)?;
                            let mut f =
                                renderer.render(&mut target, size, Transform::Normal)?;
                            let _ = Frame::render_texture_from_to(
                                &mut f,
                                blurred,
                                gpublur::GpuBlur::half_src(dst),
                                dst,
                                &[damage],
                                &[],
                                Transform::Normal,
                                1.0,
                            );
                            let _ = f.finish();
                        }
                    }

                    let mut target = renderer.bind(scene)?;
                    let mut f = renderer.render(&mut target, size, Transform::Normal)?;
                    draw_render_elements(&mut f, 1.0, elements, &[damage])?;

                    let radius = current
                        .corner_radius
                        .min(rect.size.w / 2)
                        .min(rect.size.h / 2)
                        .max(0);
                    if let Some(backdrop) = &ctx.backdrop {
                        chrome.round_corners(&mut f, &backdrop.sharp, *rect, radius, logical_size);
                    }
                    if *focused {
                        chrome.focus_ring(
                            &mut f,
                            *rect,
                            radius,
                            current.ring_width,
                            cusk::theme::ACCENT,
                        );
                    }
                    let _ = f.finish();
                }

            }
            if let Some(scene) = held {
                blur.put_scene(scene);
            }
        }

        let mut frame = renderer.render(framebuffer, size, transform)?;
        frame.clear(Color32F::new(0.05, 0.06, 0.09, 1.0), &[damage])?;

        if live_blur {
            // Everything was composited offscreen; one blit brings it to
            // the screen. The output transform is applied here and only
            // here — the offscreen passes all render `Normal`, so applying
            // it twice would put the desktop back upside down.
            if let Some(scene) = blur.scene_ref() {
                Frame::render_texture_from_to(
                    &mut frame,
                    scene,
                    Rectangle::<f64, smithay::utils::Buffer>::from_size(Size::from((
                        size.w as f64,
                        size.h as f64,
                    ))),
                    Rectangle::from_size(size),
                    &[damage],
                    &[],
                    Transform::Normal,
                    1.0,
                )?;
            }
        }

        if !live_blur {
        if let Some(backdrop) = &ctx.backdrop {
            let whole = Rectangle::from_size(logical_size);
            // Called through the trait explicitly: `GlesFrame` has an
            // inherent method of the same name taking two extra shader
            // arguments, and it shadows the trait one.
            Frame::render_texture_from_to(
                &mut frame,
                &backdrop.sharp,
                texture_src(whole),
                Rectangle::from_size(size),
                &[damage],
                &[],
                Transform::Normal,
                1.0,
            )?;
        }

        // Back to front. `Space::elements` yields bottom-first, which is
        // the order to *draw* in — the opposite of what
        // `draw_render_elements` expects for a combined list, and the
        // reason the previous flattened version stacked windows upside
        // down whenever two of them overlapped.
        for (rect, focused, elements) in &layers {
            if let Some(backdrop) = &ctx.backdrop {
                if current.blur {
                    // Both textures are output-sized, so a window's
                    // rectangle is its own source crop with no conversion.
                    if let Some(clipped) = rect.intersection(Rectangle::from_size(logical_size)) {
                        Frame::render_texture_from_to(
                            &mut frame,
                            &backdrop.blurred,
                            texture_src(clipped),
                            to_physical(clipped),
                            &[damage],
                            &[],
                            Transform::Normal,
                            1.0,
                        )?;
                    }
                }
            }
            draw_render_elements(&mut frame, 1.0, elements, &[damage])?;

            // Corners are painted back over the window that was just
            // drawn, so they must come after it and before the next one —
            // another reason the per-window loop replaced a flat list.
            //
            // Clamped to half the shorter side: at any more than that,
            // opposite corner patches overlap and erase the middle of the
            // window.
            let radius = current
                .corner_radius
                .min(rect.size.w / 2)
                .min(rect.size.h / 2)
                .max(0);
            match &ctx.backdrop {
                Some(backdrop) => {
                    chrome.round_corners(&mut frame, &backdrop.sharp, *rect, radius, logical_size)
                }
                // Rounding works by painting the wallpaper back over the
                // square corner, so with no wallpaper there is nothing to
                // paint and corners stay square. Said once, because
                // "I set corner-radius and nothing happened" is otherwise
                // a mystery with no evidence anywhere.
                None if radius > 0 && !ctx.warned_square_corners => {
                    ctx.warned_square_corners = true;
                    tracing::info!(
                        "corner-radius needs appearance.wallpaper: corners are rounded by \
                         painting the wallpaper back over them"
                    );
                }
                None => {}
            }
            if *focused {
                chrome.focus_ring(
                    &mut frame,
                    *rect,
                    radius,
                    current.ring_width,
                    cusk::theme::ACCENT,
                );
            }
        }


        }


        // Drawn after the windows and before the cursor. A floating window
        // can still be dragged over the reserved strip — only tiling is
        // obliged to respect it — so the bar has to be painted over the
        // top or it disappears under the first window someone moves up.
        if state.panel_height > 0 {
            let output = Size::from((logical_size.w, logical_size.h));
            let bar = to_physical(panel::panel_area(output, state.panel_height));
            let bg = cusk::theme::premultiplied([
                cusk::theme::BG[0],
                cusk::theme::BG[1],
                cusk::theme::BG[2],
                0.85,
            ]);
            frame.draw_solid(bar, &[damage], Color32F::new(bg[0], bg[1], bg[2], bg[3]))?;

            let active = state.workspaces.active_index();
            let occupied = state.workspaces.occupied();
            for (index, pill) in
                panel::pills(output, state.panel_height, state.workspaces.len(), active)
                    .into_iter()
                    .enumerate()
            {
                // Three states, and the empty one is the point: a
                // workspace with nothing on it has to look different from
                // one that does, or switching to it still looks like a
                // hang.
                let colour = if index == active {
                    cusk::theme::ACCENT
                } else if occupied.get(index).copied().unwrap_or(false) {
                    [
                        cusk::theme::TEXT_DIM[0],
                        cusk::theme::TEXT_DIM[1],
                        cusk::theme::TEXT_DIM[2],
                        0.75,
                    ]
                } else {
                    [
                        cusk::theme::SURFACE_HI[0],
                        cusk::theme::SURFACE_HI[1],
                        cusk::theme::SURFACE_HI[2],
                        0.55,
                    ]
                };
                let c = cusk::theme::premultiplied(colour);
                frame.draw_solid(
                    to_physical(pill),
                    &[damage],
                    Color32F::new(c[0], c[1], c[2], c[3]),
                )?;
            }

            // The title, prepared before the frame — see `title` above.
            if let Some((rect, image_size, texture)) = &title {
                Frame::render_texture_from_to(
                    &mut frame,
                    texture,
                    Rectangle::<f64, smithay::utils::Buffer>::from_size(Size::from((
                        image_size.0 as f64,
                        image_size.1 as f64,
                    ))),
                    to_physical(*rect),
                    &[damage],
                    &[],
                    Transform::Normal,
                    1.0,
                )?;
            }
        }

        // The pointer is drawn last, over everything including the focus
        // ring. A cursor that can be covered by a window is a cursor you
        // lose exactly when you are trying to click something.
        match &state.cursor {
            smithay::input::pointer::CursorImageStatus::Hidden => {}

            // A client's own cursor, from the elements built above.
            smithay::input::pointer::CursorImageStatus::Surface(_) => {
                if let Some(elements) = &cursor_elements {
                    draw_render_elements(&mut frame, 1.0, elements, &[damage])?;
                }
            }

            // Nothing has an opinion, or it asked for a named shape cusk
            // does not have artwork for. Every named shape gets the arrow
            // rather than nothing: the wrong pointer is usable, no pointer
            // is not.
            smithay::input::pointer::CursorImageStatus::Named(_) => {
                if let Some(texture) = &ctx.pointer_texture {
                    let at: Point<i32, Logical> = state.pointer_location.to_i32_round();
                    let size = ctx.pointer_image.image.width as i32;
                    let dst = Rectangle::<i32, Physical>::new(
                        Point::from((
                            at.x - ctx.pointer_image.hotspot.0,
                            at.y - ctx.pointer_image.hotspot.1,
                        )),
                        Size::from((size, size)),
                    );
                    Frame::render_texture_from_to(
                        &mut frame,
                        texture,
                        Rectangle::from_size(Size::from((size as f64, size as f64))),
                        dst,
                        &[damage],
                        &[],
                        Transform::Normal,
                        1.0,
                    )?;
                }
            }
        }

        let _sync = frame.finish()?;

        let now = start.elapsed().as_millis() as u32;
        for window in state.space.elements() {
            if let Some(toplevel) = window.toplevel() {
                send_frames(toplevel.wl_surface(), now);
            }
    }
    Ok(())
}
/// Everything a driver needs to run a session, built once.
///
/// Extracted so the winit and DRM drivers construct it the same way. The
/// alternative is each backend building its own compositor state, which is
/// two places for a global to be registered and one of them to be forgotten —
/// and a missing global shows up as a client that starts and draws nothing.
struct Compositor {
    display: Display<Cusk>,
    state: Cusk,
    listener: ListeningSocket,
    socket_name: String,
    keyboard: smithay::input::keyboard::KeyboardHandle<Cusk>,
    pointer: smithay::input::pointer::PointerHandle<Cusk>,
}

/// Build the Wayland side: display, globals, seat, socket.
///
/// The dmabuf global is deliberately *not* created here. It needs the
/// renderer's format list, which only the driver has, so it is the one global
/// each backend registers for itself.
fn build_compositor(cfg: &config::Config) -> Result<Compositor, Box<dyn std::error::Error>> {
    let mod_key = ModKey::resolve(&cfg.mod_key);
    let display: Display<Cusk> = Display::new()?;
    let dh = display.handle();

    let compositor_state = CompositorState::new::<Cusk>(&dh);
    let shm_state = ShmState::new::<Cusk>(&dh, vec![]);
    let dmabuf_state = DmabufState::new();
    let mut seat_state = SeatState::new();
    let seat = seat_state.new_wl_seat(&dh, "cusk");

    let mut state = Cusk {
        compositor_state,
        xdg_shell_state: XdgShellState::new::<Cusk>(&dh),
        xdg_decoration_state: XdgDecorationState::new::<Cusk>(&dh),
        shm_state,
        dmabuf_state,
        data_device_state: DataDeviceState::new::<Cusk>(&dh),
        seat_state,
        seat,
        space: Space::default(),
        pointer_location: (0.0, 0.0).into(),
        modifiers: ModifiersState::default(),
        mod_key,
        workspaces: workspace::Workspaces::new(
            cfg.workspace_count.max(1) as usize,
            cfg.tiling_on_start,
            match cfg.default_layout.as_str() {
                "columns" => layout::Layout::Columns,
                _ => layout::Layout::MasterStack { ratio: cfg.master_ratio },
            },
        ),
        gaps: layout::Gaps { inner: cfg.inner_gap, outer: cfg.outer_gap },
        focus_follows_mouse: cfg.focus_follows_mouse,
        panel_height: cfg.panel_height,
        pending_dmabufs: Vec::new(),
        cursor: smithay::input::pointer::CursorImageStatus::default_named(),
        output_size: (1280, 800),
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
    let pointer = state.seat.add_pointer();

    Ok(Compositor { display, state, listener, socket_name, keyboard, pointer })
}
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

/// Launch a terminal as a client of this compositor.
///
/// Reaping is deliberate and not optional. A compositor that spawns children
/// and never waits accumulates zombies for every window ever closed; the
/// symptom is a process table filling up over a long session, which looks like
/// anything but a window manager bug.
fn spawn_terminal(term: &str, socket_name: &str) {
    // Only the child's environment is changed. Setting WAYLAND_DISPLAY on the
    // compositor's own process would make it a client of itself the next time
    // anything connected.
    match std::process::Command::new(term)
        .env("WAYLAND_DISPLAY", socket_name)
        .spawn()
    {
        Ok(child) => {
            tracing::info!("spawned {term} (pid {})", child.id());
            std::thread::spawn(move || {
                let mut child = child;
                let _ = child.wait();
            });
        }
        Err(e) => tracing::error!("could not spawn {term}: {e}"),
    }
}

/// Find the launcher binary.
///
/// A bare name is looked for beside cusk first, then left to `PATH`. Beside-
/// first matters during development, where the two crates build into separate
/// target directories and the one on `PATH` — if any — is a stale install.
fn resolve_launcher(name: &str) -> String {
    if name.contains('/') {
        return name.to_string();
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(sibling) = exe.parent().map(|d| d.join(name)) {
            if sibling.exists() {
                return sibling.to_string_lossy().into_owned();
            }
        }
    }
    name.to_string()
}

fn pick_terminal() -> Option<&'static str> {
    config::known_terminals().find(|t| {
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

    // Reports what the tty backend would have to drive, and exits. Safe to run
    // from inside a running desktop: it never takes DRM master.
    if args.iter().any(|a| a == "--probe-drm") {
        match tty::probe() {
            Ok((access, cards)) => tty::report(&access, &cards),
            Err(e) => {
                println!("\n  {e}\n");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    // Proves the GPU path the tty render loop will need. Uses the render node,
    // so it needs no session and runs anywhere.
    if args.iter().any(|a| a == "--probe-render") {
        match tty::probe_render() {
            Ok(message) => println!("\n  {message}\n"),
            Err(e) => {
                println!("\n  {e}\n");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    // Renders with the GPU and scans that buffer out — the two halves joined.
    if args.iter().any(|a| a == "--probe-scanout") {
        let seconds = args
            .iter()
            .find_map(|a| a.strip_prefix("--seconds="))
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);
        match tty::probe_scanout(seconds) {
            Ok(()) => println!("\n  the GPU drew it and the display scanned it out.\n"),
            Err(e) => {
                println!("\n  {e}\n");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    // The real thing: cusk on a virtual terminal.
    if args.iter().any(|a| a == "--tty") {
        let seconds = args
            .iter()
            .find_map(|a| a.strip_prefix("--seconds="))
            .and_then(|v| v.parse().ok())
            .unwrap_or(30);
        let config_path = config::default_path();
        // Reported, not swallowed. This used to be `unwrap_or_default()`, and
        // a config with a duplicate key silently became *every* setting at its
        // default — no wallpaper, default panel, default everything — with
        // nothing on screen or in the log to say why. A file that fails to
        // parse is the one case where the user most needs to be told.
        let cfg = match config::Config::load(&config_path) {
            Ok((cfg, complaints)) => {
                for complaint in &complaints {
                    tracing::warn!("{}: {}", config_path.display(), complaint);
                }
                cfg
            }
            Err(e) => {
                println!();
                println!("  {} could not be read:", config_path.display());
                println!("    {e}");
                println!();
                println!("  Every setting is at its default until that is fixed.");
                println!("  Duplicate keys and repeated [section] headers are the usual cause.");
                println!();
                config::Config::default()
            }
        };
        match run_on_tty(cfg, seconds) {
            Ok(()) => println!("\n  cusk exited cleanly.\n"),
            Err(e) => {
                println!("\n  {e}\n");
                std::process::exit(1);
            }
        }
        return Ok(());
    }

    // Sets a real mode and takes DRM master, so it cannot run beside another
    // compositor. Bounded by a watchdog that restores the display and exits
    // whatever the main thread is doing.
    if args.iter().any(|a| a == "--modeset-test") {
        let seconds = args
            .iter()
            .find_map(|a| a.strip_prefix("--seconds="))
            .and_then(|v| v.parse().ok())
            .unwrap_or(5);
        match tty::modeset(seconds) {
            Ok(()) => println!("\n  mode set and restored cleanly.\n"),
            Err(e) => {
                println!("\n  {e}\n");
                std::process::exit(1);
            }
        }
        return Ok(());
    }
    let requested: Option<String> =
        args.iter().find(|a| !a.starts_with('-')).cloned();

    // Loaded before anything else is built, because it decides how things get
    // built. A missing file is written with the schema's own documentation
    // rather than left absent: a config nobody can find is a config nobody
    // edits, and the generated file is the discoverability half of §4 until
    // the GUI exists.
    let config_path = config::default_path();
    if !config_path.exists() {
        if let Some(dir) = config_path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        match std::fs::write(&config_path, config::Config::default_file()) {
            Ok(()) => tracing::info!("wrote default config to {}", config_path.display()),
            Err(e) => tracing::warn!("could not write {}: {e}", config_path.display()),
        }
    }
    let cfg = match config::Config::load(&config_path) {
        Ok((cfg, complaints)) => {
            // Surfaced, not swallowed. §4 asks for validation errors to be
            // reported rather than silently reverted, and a setting that did
            // nothing with no explanation is the complaint this prevents.
            for complaint in &complaints {
                tracing::warn!("{}: {}", config_path.display(), complaint);
            }
            cfg
        }
        Err(e) => {
            tracing::warn!("{}: {e} — using defaults", config_path.display());
            config::Config::default()
        }
    };

    let mod_key = ModKey::resolve(&cfg.mod_key);

    let Compositor {
        mut display,
        mut state,
        listener,
        socket_name,
        keyboard,
        pointer,
    } = build_compositor(&cfg)?;
    let mut dh = display.handle();

    let face = text::find_font(&cfg.font).and_then(|path| {
        let loaded = text::Face::load(&path);
        match &loaded {
            Some(_) => tracing::info!("font {}", path.display()),
            None => tracing::warn!("could not load font {}", path.display()),
        }
        loaded
    });
    if face.is_none() {
        tracing::warn!("no usable font; the panel will show no title");
    }
    let mut ctx = FrameContext {
        chrome: None,
        blur: None,
        backdrop: None,
        refused: None,
        face,
        title_texture: None,
        pointer_image: cursor::arrow(24),
        pointer_texture: None,
        warned_square_corners: false,
    };

    let mut watcher = config::Watcher::new(config_path.clone());
    let mut current = cfg.clone();

    let (mut backend, mut winit_loop) = winit::init::<GlesRenderer>()?;

    // Advertised only if the renderer can actually import. A dmabuf global that
    // rejects every buffer is worse than none: clients see the protocol, try
    // it, fail, and fall back — having paid for the round trip and, for some,
    // having already given up on shared memory.
    {
        let formats: Vec<_> = backend
            .renderer()
            .egl_context()
            .dmabuf_render_formats()
            .iter()
            .copied()
            .collect();
        if formats.is_empty() {
            tracing::warn!(
                "no dmabuf render formats; clients will keep falling back to shared memory"
            );
        } else {
            let count = formats.len();

            // Version 4, with default feedback, and the feedback is the point.
            // A v3 global carries formats but no device, and Mesa's Wayland EGL
            // learns *which render node to open* from the feedback's main
            // device. Without it a client cannot find a GPU, reports
            // `failed to get driver name for fd -1`, and falls back to software
            // — which is the exact symptom this milestone exists to remove.
            // Advertising v3 alone was measured to change nothing.
            let device = smithay::backend::egl::EGLDevice::device_for_display(
                backend.renderer().egl_context().display(),
            )
            .ok()
            .and_then(|device| device.render_device_path().ok())
            .and_then(|path| {
                use std::os::unix::fs::MetadataExt;
                std::fs::metadata(&path).ok().map(|meta| (path, meta.rdev()))
            });

            match device {
                Some((path, dev)) => {
                    let feedback = DmabufFeedbackBuilder::new(dev, formats).build()?;
                    let _global = state
                        .dmabuf_state
                        .create_global_with_default_feedback::<Cusk>(&dh, &feedback);
                    tracing::info!(
                        "dmabuf advertised with {count} formats on {}",
                        path.display()
                    );
                }
                None => {
                    // Falling back to v3 rather than to nothing: a client that
                    // already knows its device can still use the format list.
                    let _global = state.dmabuf_state.create_global::<Cusk>(&dh, formats);
                    tracing::warn!(
                        "dmabuf advertised with {count} formats but no device node; \
                         clients that cannot guess a render node will use shared memory"
                    );
                }
            }
        }
    }

    let start = std::time::Instant::now();

    // Resolved once, outside the startup branch, because the spawn keybinding
    // needs it for the whole life of the session — not just at boot. Owned
    // rather than borrowed: `pick_terminal` yields &'static str and `requested`
    // a local String, and unifying the two by reference makes the local's
    // lifetime the binding constraint for no gain.
    let mut terminal: Option<String> = requested
        .clone()
        .or_else(|| match cfg.terminal.as_str() {
            // "auto" is a strategy, not a program: probe the schema's list in
            // preference order. A named terminal is taken at its word even if
            // absent, so a typo'd or uninstalled choice fails loudly at spawn
            // rather than silently starting something else.
            "auto" => pick_terminal().map(str::to_owned),
            named => Some(named.to_string()),
        });

    if !no_spawn {
        match terminal.as_deref() {
            Some(term) => spawn_terminal(term, &socket_name),
            None => tracing::warn!(
                "no terminal found — start one by hand with \
                 WAYLAND_DISPLAY={socket_name}"
            ),
        }
    } else {
        tracing::info!("--no-spawn: no client started");
    }

    // Printed rather than logged, and printed once. The socket name is the one
    // thing someone testing interactively needs from a second terminal, and
    // "listening on cusk-1" buried in a log line is easy to miss — connecting
    // to a compositor that has since exited fails as NoCompositor, which reads
    // like a client bug rather than a stale socket.
    for line in [
        String::new(),
        "  cusk is running. From another terminal, while this one stays open:".into(),
        String::new(),
        format!("      WAYLAND_DISPLAY={socket_name} alacritty"),
        String::new(),
        format!(
            "  bindings use {}   (set CUSK_MOD={} if the host eats it)",
            mod_key.label(),
            // Suggesting the modifier already in use is advice that cannot
            // help, and reads as the hint being boilerplate.
            if matches!(mod_key, ModKey::Alt) { "ctrl-alt" } else { "alt" },
        ),
        String::new(),
        "      click           focus and raise".into(),
        format!("      {} + drag     move", mod_key.label()),
        format!("      {} + right    resize from the nearest corner", mod_key.label()),
        format!("      {} + m        maximise / restore", mod_key.label()),
        format!("      {} + t        tiling on / off", mod_key.label()),
        format!("      {} + e        cycle layout (master-stack, columns)", mod_key.label()),
        format!("      {} + h / l    narrow / widen the master column", mod_key.label()),
        format!("      {} + space    float this window out of the layout", mod_key.label()),
        format!("      {} + enter    open another terminal", mod_key.label()),
        format!("      {} + d        application launcher", mod_key.label()),
        format!("      {} + j / k    focus next / previous window", mod_key.label()),
        format!("      {} + shift + j / k", mod_key.label()),
        "                      move it earlier / later in the layout".into(),
        format!("      {} + shift + p    promote it to master", mod_key.label()),
        format!("      {} + 1..9      switch workspace", mod_key.label()),
        format!("      {} + shift + 1..9", mod_key.label()),
        "                      send this window to that workspace".into(),
        "      close window    quit".into(),
        String::new(),
    ] {
        println!("{line}");
    }

    let mut clients = Vec::new();
    // Diagnostics for the input path. Two independent questions -- does winit
    // deliver pointer events, and does hit-testing find a surface -- that look
    // identical from the outside when either is false.
    let mut motions: u64 = 0;
    let mut motions_with_surface: u64 = 0;
    let mut last_report = std::time::Instant::now();

    loop {
        // Read before dispatch: absolute pointer positions arrive normalised
        // and need the output size to become coordinates.
        //
        // The backend reports physical pixels; pointer routing and the Space
        // both work in logical ones. At scale 1 the numbers are identical,
        // which is exactly why this conversion has to be explicit — it will be
        // silently wrong on the first HiDPI output otherwise.
        let output_size = backend.window_size();
        let logical_size = output_size.to_f64().to_logical(1.0).to_i32_round();
        // Relayout on resize, not just on window changes — otherwise tiles keep
        // the old output's dimensions and either overhang the window or leave a
        // margin, both of which read as a layout bug rather than a stale size.
        if state.output_size != (logical_size.w, logical_size.h) {
            state.output_size = (logical_size.w, logical_size.h);
            state.relayout();
        }

        let status = winit_loop.dispatch_new_events(|event| match event {
            WinitEvent::Input(InputEvent::Keyboard { event }) => {
                let serial = SERIAL_COUNTER.next_serial();
                let time = event.time_msec();
                let key_state = event.state();
                let binding = keyboard.input::<Option<Binding>, _>(
                    &mut state,
                    event.key_code(),
                    event.state(),
                    serial,
                    time,
                    |state, modifiers, handle| {
                        // Modifiers are only offered here, and compositor
                        // bindings need them at button time. Cache rather than
                        // ask the seat later, which would report the state as
                        // of now instead of as of the click.
                        state.modifiers = *modifiers;

                        // Intercepted, not forwarded: a binding the compositor
                        // acts on must not also reach the client, or the
                        // terminal receives an 'm' every time a window is
                        // maximised.
                        if key_state == KeyState::Pressed && state.mod_key.held(modifiers) {
                            let bound = binding_for(handle.modified_sym(), modifiers.shift);
                            if let Some(binding) = bound {
                                return FilterResult::Intercept(Some(binding));
                            }
                        }
                        FilterResult::Forward
                    },
                );

                if let Some(Some(binding)) = binding {
                    state.apply_binding(binding, &current, terminal.as_deref(), &socket_name, (logical_size.w, logical_size.h));
                }
            }

            WinitEvent::Input(InputEvent::PointerMotionAbsolute { event }) => {
                let location = event.position_transformed(logical_size);
                state.pointer_location = location;
                let hit = state.surface_under(location);
                motions += 1;
                if hit.is_some() {
                    motions_with_surface += 1;
                }
                if last_report.elapsed() >= std::time::Duration::from_secs(2) {
                    tracing::debug!(
                        "pointer: {motions} motions, {motions_with_surface} hit a surface"
                    );
                    last_report = std::time::Instant::now();
                }
                if state.focus_follows_mouse {
                    state.focus_under_pointer(location);
                }

                let under = hit.map(|(_, surface, loc)| (surface, loc));
                pointer.motion(
                    &mut state,
                    under,
                    &MotionEvent {
                        location,
                        serial: SERIAL_COUNTER.next_serial(),
                        time: event.time_msec(),
                    },
                );
                pointer.frame(&mut state);
            }

            // Scroll, so a terminal under winit behaves like one under the tty
            // driver. Neither backend had it: an audit for driver parity found
            // it missing from *both*, which makes it a gap rather than a
            // divergence.
            WinitEvent::Input(InputEvent::PointerAxis { event }) => {
                use smithay::backend::input::{Axis, AxisSource, PointerAxisEvent};

                let source = match event.source() {
                    AxisSource::Finger => tty::ScrollSource::Finger,
                    AxisSource::Continuous => tty::ScrollSource::Continuous,
                    // Wheel and WheelTilt both carry discrete steps, which is
                    // the distinction that matters to a client.
                    _ => tty::ScrollSource::Wheel,
                };
                let scroll = tty::Scroll {
                    source,
                    horizontal: event.amount(Axis::Horizontal).unwrap_or(0.0),
                    vertical: event.amount(Axis::Vertical).unwrap_or(0.0),
                    v120: match source {
                        tty::ScrollSource::Wheel => Some((
                            event.amount_v120(Axis::Horizontal).unwrap_or(0.0),
                            event.amount_v120(Axis::Vertical).unwrap_or(0.0),
                        )),
                        _ => None,
                    },
                };
                if !scroll.is_empty() {
                    pointer.axis(&mut state, axis_frame(scroll, event.time_msec()));
                    pointer.frame(&mut state);
                }
            }

            WinitEvent::Input(InputEvent::PointerButton { event }) => {
                let serial = SERIAL_COUNTER.next_serial();
                let button = event.button_code();
                let button_state = event.state();
                let mut forward = true;
                if button_state == ButtonState::Pressed {
                    forward = state.press(button, serial);
                }

                // A binding the compositor consumed must not also reach the
                // client — Super+drag would otherwise select text in the
                // terminal it is dragging.
                if forward {
                    pointer.button(
                        &mut state,
                        &ButtonEvent {
                            button,
                            state: button_state,
                            serial,
                            time: event.time_msec(),
                        },
                    );
                    pointer.frame(&mut state);
                }
            }

            _ => {}
        });

        match watcher.poll() {
            config::Reload::Unchanged => {}
            config::Reload::Applied { config: fresh, complaints } => {
                for complaint in &complaints {
                    tracing::warn!("{}: {}", watcher.path().display(), complaint);
                }
                // Named, not silently skipped. A setting that was edited and
                // did nothing, with nothing said about it, is the worst thing
                // hot reload can do.
                for key in config::restart_only_changes(&current, &fresh) {
                    tracing::info!("{key} changed; takes effect on restart");
                }
                state.apply_config(&fresh);
                terminal = match fresh.terminal.as_str() {
                    "auto" => pick_terminal().map(str::to_owned),
                    named => Some(named.to_string()),
                };
                tracing::info!("reloaded {}", watcher.path().display());
                current = fresh;
            }
            config::Reload::Failed(e) => {
                tracing::warn!(
                    "{}: {e} — keeping the running configuration",
                    watcher.path().display()
                );
            }
        }

        if let PumpStatus::Exit(_) = status {
            tracing::info!("window closed, exiting");
            return Ok(());
        }

        let size = output_size;
        let damage = Rectangle::from_size(size);
        {
            let (renderer, mut framebuffer) = backend.bind()?;

            draw_frame(
                renderer,
                &mut framebuffer,
                &mut state,
                &mut ctx,
                &current,
                size,
                logical_size,
                Transform::Flipped180,
                start,
            )?;

            if let Some(stream) = listener.accept()? {
                let client = dh.insert_client(stream, Arc::new(ClientState::default()))?;
                clients.push(client);
            }

            display.dispatch_clients(&mut state)?;
            display.flush_clients()?;
        }

        // Reported from the frame loop, not the motion handler: with the
        // pointer outside the window no motion arrives, and the one moment the
        // geometry most needs checking is when nothing is happening.
        if last_report.elapsed() >= std::time::Duration::from_secs(2) {
            let geom: Vec<String> = state
                .space
                .elements()
                .map(|w| format!("{:?}", w.bbox().size))
                .collect();
            tracing::debug!("windows={} bboxes={geom:?} motions={motions} hits={motions_with_surface}",
                            state.space.elements().count());
            last_report = std::time::Instant::now();
        }

        // After flushing: submit can block, and a client waiting on a message
        // still sitting in our buffer would be waiting on us.
        backend.submit(Some(&[damage]))?;
    }
}
