//! Open an `xdg_popup` and report whether the compositor configures it.
//!
//! `cargo run --example popupprobe` against a running cusk.
//!
//! Exists because the alternative was to right-click in Dolphin and look,
//! which is not a test and is how xdg_popup managed to be entirely
//! unimplemented in this compositor without anyone finding it in code review.
//! Every other probe here drives a protocol the dock speaks; this one drives a
//! protocol the dock does not, purely to ask the compositor a question that
//! needs a mouse to ask any other way.
//!
//! It checks the one thing that cannot be faked: a popup cannot attach a
//! buffer until it has been configured, so `xdg_popup.configure` arriving is
//! the difference between a menu that can exist and one that cannot.

use std::time::{Duration, Instant};

use wayland_client::protocol::{
    wl_buffer::WlBuffer, wl_compositor::WlCompositor, wl_registry, wl_seat::WlSeat, wl_shm,
    wl_shm_pool::WlShmPool, wl_surface::WlSurface,
};
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols::xdg::shell::client::{
    xdg_popup::{self, XdgPopup},
    xdg_positioner::XdgPositioner,
    xdg_surface::{self, XdgSurface},
    xdg_toplevel::{self, XdgToplevel},
    xdg_wm_base::{self, XdgWmBase},
};

/// Long enough for a round trip through the compositor's commit handler, which
/// is where the popup's first configure is sent from.
const PATIENCE: Duration = Duration::from_secs(3);

#[derive(Default)]
struct State {
    compositor: Option<WlCompositor>,
    wm_base: Option<XdgWmBase>,
    shm: Option<wl_shm::WlShm>,
    seat: Option<WlSeat>,

    /// Set when the parent toplevel has been configured, which is the point at
    /// which a popup may be created at all.
    parent_configured: bool,
    parent_xdg: Option<XdgSurface>,

    popup_xdg: Option<XdgSurface>,
    /// The finding.
    popup_configured: bool,
    /// Geometry from the popup's configure, to check it was constrained rather
    /// than echoed back.
    popup_geometry: Option<(i32, i32, i32, i32)>,
    /// A compositor that dismisses the popup immediately is a different
    /// failure from one that never configures it, and worth telling apart.
    popup_done: bool,
}

fn main() {
    let Ok(connection) = Connection::connect_to_env() else {
        eprintln!("popupprobe: no Wayland connection");
        std::process::exit(2);
    };
    let mut queue = connection.new_event_queue();
    let qh = queue.handle();
    connection.display().get_registry(&qh, ());

    let mut state = State::default();
    let _ = queue.roundtrip(&mut state);

    let (Some(compositor), Some(wm_base), Some(shm)) =
        (state.compositor.clone(), state.wm_base.clone(), state.shm.clone())
    else {
        eprintln!("popupprobe: compositor is missing wl_compositor, xdg_wm_base or wl_shm");
        std::process::exit(2);
    };

    // ── The parent window ────────────────────────────────────────────────
    let parent = compositor.create_surface(&qh, ());
    let parent_xdg = wm_base.get_xdg_surface(&parent, &qh, Role::Parent);
    let toplevel = parent_xdg.get_toplevel(&qh, ());
    toplevel.set_title("popupprobe".into());
    toplevel.set_app_id("cusk-popupprobe".into());
    state.parent_xdg = Some(parent_xdg.clone());
    parent.commit();

    // A toplevel may not attach a buffer before its first configure, so this
    // waits rather than assuming.
    if !wait_for(&mut queue, &mut state, PATIENCE, |s| s.parent_configured) {
        println!("  FAIL  the parent window was never configured");
        println!("        nothing further can be tested — a popup needs a mapped parent");
        std::process::exit(1);
    }
    println!("  PASS  parent window configured");

    let buffer = paint(&shm, &qh, 400, 300);
    parent.attach(Some(&buffer), 0, 0);
    parent.damage(0, 0, 400, 300);
    parent.commit();
    let _ = queue.roundtrip(&mut state);

    // ── The popup ────────────────────────────────────────────────────────
    //
    // Anchored to the bottom-right of a small rectangle near the parent's own
    // bottom-right corner, and asking to slide if it does not fit. A menu that
    // fits everywhere would not exercise `unconstrain_popup` at all.
    let positioner = wm_base.create_positioner(&qh, ());
    positioner.set_size(200, 250);
    positioner.set_anchor_rect(380, 280, 1, 1);
    positioner.set_anchor(wayland_protocols::xdg::shell::client::xdg_positioner::Anchor::BottomRight);
    positioner.set_gravity(wayland_protocols::xdg::shell::client::xdg_positioner::Gravity::BottomRight);
    positioner.set_constraint_adjustment(
        wayland_protocols::xdg::shell::client::xdg_positioner::ConstraintAdjustment::SlideX
            | wayland_protocols::xdg::shell::client::xdg_positioner::ConstraintAdjustment::SlideY
            | wayland_protocols::xdg::shell::client::xdg_positioner::ConstraintAdjustment::FlipY,
    );

    let popup_surface = compositor.create_surface(&qh, ());
    let popup_xdg = wm_base.get_xdg_surface(&popup_surface, &qh, Role::Popup);
    let popup = popup_xdg.get_popup(Some(&parent_xdg), &positioner, &qh, ());
    if let Some(seat) = &state.seat {
        // Requested, so the compositor's `grab` handler is exercised. cusk
        // declines to take a protocol grab and must decline *quietly* — a
        // compositor that errors here kills the client, which would show up
        // as this probe dying rather than reporting.
        popup.grab(seat, 0);
    }
    state.popup_xdg = Some(popup_xdg.clone());
    popup_surface.commit();

    let configured = wait_for(&mut queue, &mut state, PATIENCE, |s| {
        s.popup_configured || s.popup_done
    });

    let mut failures = 0;

    if !configured || !state.popup_configured {
        println!("  FAIL  the popup was never configured");
        println!("        a popup cannot attach a buffer before its first configure,");
        println!("        so on this compositor no menu can appear at all");
        failures += 1;
    } else {
        println!("  PASS  popup configured");

        match state.popup_geometry {
            Some((x, y, w, h)) => {
                println!("        geometry {w}x{h} at ({x}, {y})");
                if w > 0 && h > 0 {
                    println!("  PASS  it was given a size");
                } else {
                    println!("  FAIL  configured with an empty rectangle");
                    failures += 1;
                }
            }
            None => {
                println!("  FAIL  configured with no geometry");
                failures += 1;
            }
        }

        // The popup was asked for at the parent's far corner with a size that
        // cannot fit there. A compositor that never unconstrains echoes the
        // request back unchanged and the menu hangs off the screen.
        if state.popup_geometry.is_some_and(|(x, y, _, _)| x != 380 || y != 280) {
            println!("  PASS  and it was moved to fit, rather than echoed back");
        } else {
            println!("  WARN  positioned exactly as asked — either it fits here,");
            println!("        or the positioner is not being unconstrained");
        }

        // Now the thing the configure was for.
        let buffer = paint(&shm, &qh, 200, 250);
        popup_surface.attach(Some(&buffer), 0, 0);
        popup_surface.damage(0, 0, 200, 250);
        popup_surface.commit();
        let _ = queue.roundtrip(&mut state);

        // A frame callback proves the compositor is driving the popup, which
        // is what a menu needs to repaint a hover highlight. This was the
        // exact omission that made every layer surface look frozen.
        let mut drew = false;
        popup_surface.frame(&qh, ());
        popup_surface.commit();
        let start = Instant::now();
        while start.elapsed() < PATIENCE && !drew {
            let _ = queue.blocking_dispatch(&mut state);
            drew = FRAME.with(|f| f.get());
        }
        if drew {
            println!("  PASS  and it receives frame callbacks");
        } else {
            println!("  FAIL  no frame callback — the menu would draw once and freeze");
            failures += 1;
        }
    }

    if state.popup_done {
        println!("  WARN  the compositor dismissed the popup during the test");
    }

    if failures == 0 {
        println!("\npopups work");
    } else {
        println!("\n{failures} failure(s)");
        std::process::exit(1);
    }
}

/// Which role an `xdg_surface` was created for.
///
/// `xdg_surface.configure` arrives on the same interface for both, and the
/// only way to know which one it is about is to have recorded it — so the role
/// is the object's user data rather than something inferred later.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Parent,
    Popup,
}

fn wait_for(
    queue: &mut wayland_client::EventQueue<State>,
    state: &mut State,
    patience: Duration,
    done: impl Fn(&State) -> bool,
) -> bool {
    let start = Instant::now();
    while start.elapsed() < patience {
        if done(state) {
            return true;
        }
        if queue.blocking_dispatch(state).is_err() {
            return done(state);
        }
    }
    done(state)
}

/// An opaque buffer of the given size.
///
/// Contents do not matter — nothing looks at this — but the buffer must exist,
/// because a surface with no buffer is not mapped and an unmapped parent
/// cannot carry a popup.
fn paint(shm: &wl_shm::WlShm, qh: &QueueHandle<State>, w: i32, h: i32) -> WlBuffer {
    use rustix::fs::{ftruncate, memfd_create, MemfdFlags};
    let len = (w * h * 4) as usize;
    let fd = memfd_create("popupprobe", MemfdFlags::CLOEXEC).expect("a memfd");
    ftruncate(&fd, len as u64).expect("room for the buffer");
    let pool: WlShmPool = shm.create_pool(std::os::fd::AsFd::as_fd(&fd), len as i32, qh, ());
    let buffer = pool.create_buffer(0, w, h, w * 4, wl_shm::Format::Xrgb8888, qh, ());
    pool.destroy();
    buffer
}

thread_local! {
    /// Set by the frame callback. A thread-local rather than a `State` field
    /// only because `wl_callback` is dispatched with `()` user data here and
    /// threading a flag through would not make it clearer.
    static FRAME: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        qh: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global { name, interface, version } = event else { return };
        match interface.as_str() {
            "wl_compositor" => {
                state.compositor =
                    Some(registry.bind::<WlCompositor, _, _>(name, version.min(4), qh, ()))
            }
            "xdg_wm_base" => {
                state.wm_base =
                    Some(registry.bind::<XdgWmBase, _, _>(name, version.min(3), qh, ()))
            }
            "wl_shm" => state.shm = Some(registry.bind::<wl_shm::WlShm, _, _>(name, 1, qh, ())),
            "wl_seat" => {
                state.seat = Some(registry.bind::<WlSeat, _, _>(name, version.min(5), qh, ()))
            }
            _ => {}
        }
    }
}

impl Dispatch<XdgWmBase, ()> for State {
    fn event(
        _: &mut Self,
        wm_base: &XdgWmBase,
        event: xdg_wm_base::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // Unanswered pings are how a compositor decides a client has hung. A
        // probe that failed to pong would be killed mid-test and look like a
        // compositor bug.
        if let xdg_wm_base::Event::Ping { serial } = event {
            wm_base.pong(serial);
        }
    }
}

impl Dispatch<XdgSurface, Role> for State {
    fn event(
        state: &mut Self,
        surface: &XdgSurface,
        event: xdg_surface::Event,
        role: &Role,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let xdg_surface::Event::Configure { serial } = event {
            surface.ack_configure(serial);
            match role {
                Role::Parent => state.parent_configured = true,
                // The xdg_surface configure follows the xdg_popup one and is
                // what makes the popup's state current. Recorded separately
                // from `popup_configured`, which is set by the popup's own
                // event, so "configured with no geometry" is distinguishable
                // from "not configured".
                Role::Popup => {}
            }
        }
    }
}

impl Dispatch<XdgPopup, ()> for State {
    fn event(
        state: &mut Self,
        _: &XdgPopup,
        event: xdg_popup::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            xdg_popup::Event::Configure { x, y, width, height } => {
                state.popup_configured = true;
                state.popup_geometry = Some((x, y, width, height));
            }
            xdg_popup::Event::PopupDone => state.popup_done = true,
            _ => {}
        }
    }
}

impl Dispatch<wayland_client::protocol::wl_callback::WlCallback, ()> for State {
    fn event(
        _: &mut Self,
        _: &wayland_client::protocol::wl_callback::WlCallback,
        _: wayland_client::protocol::wl_callback::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        FRAME.with(|f| f.set(true));
    }
}

impl Dispatch<XdgToplevel, ()> for State {
    fn event(
        _: &mut Self,
        _: &XdgToplevel,
        _: xdg_toplevel::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

macro_rules! ignore {
    ($($ty:ty),* $(,)?) => {$(
        impl Dispatch<$ty, ()> for State {
            fn event(
                _: &mut Self,
                _: &$ty,
                _: <$ty as Proxy>::Event,
                _: &(),
                _: &Connection,
                _: &QueueHandle<Self>,
            ) {
            }
        }
    )*};
}

ignore!(WlCompositor, WlSurface, WlSeat, WlShmPool, WlBuffer, wl_shm::WlShm, XdgPositioner);
