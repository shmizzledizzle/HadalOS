//! What windows are open, over `zwlr_foreign_toplevel_management_v1`.
//!
//! The client half of `cusk::foreign_toplevel`. cusk serves the protocol and
//! this reads it, which is what turns the dock from a launcher into a taskbar:
//! before it, the dock knew the `.desktop` files it was configured with and
//! nothing about the session it was sitting in.
//!
//! # A second connection, on its own thread
//!
//! iced owns the main Wayland connection through `iced_layershell` and does not
//! expose the registry, so this opens its **own** connection to the same
//! compositor. That is ordinary — a client may connect as many times as it
//! likes — and it is the same shape as `tray.rs`: a background thread that owns
//! a protocol, publishing a snapshot the UI thread clones.
//!
//! The alternative would be threading a wlr-foreign-toplevel handler through
//! iced's event loop, which means patching `iced_layershell`. A second socket
//! is cheaper than a fork of the windowing layer.
//!
//! # Events are deltas, and `done` is what makes them a state
//!
//! The protocol does not send "here is the window". It sends `title`, `app_id`,
//! `state`, `output_enter` — and then `done`, which means "that batch is
//! complete". A client that redraws per event shows a window with a title and
//! no app id for one frame; a client that ignores `done` never knows when it has
//! the whole picture.
//!
//! So handles accumulate into a **pending** record and are only published on
//! `done`. That is the same reason `cusk::toplevel` exists on the server side,
//! facing the other way.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use wayland_client::protocol::wl_registry;
use wayland_client::{Connection, Dispatch, Proxy, QueueHandle};
use wayland_protocols_wlr::foreign_toplevel::v1::client::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};

use crate::stage::{Thumbnail, Thumbs};
use crate::stage_protocol::{
    hadal_stage_manager_v1::HadalStageManagerV1,
    hadal_stage_thumbnail_v1::{self, HadalStageThumbnailV1},
};

/// One open window, as far as drawing is concerned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Window {
    /// Stable for the window's lifetime. Assigned here rather than taken from
    /// the protocol, which identifies windows by object and not by number.
    pub id: u32,
    pub title: String,
    pub app_id: String,
    pub activated: bool,
    pub minimized: bool,
    pub maximized: bool,
    pub fullscreen: bool,
}

impl Window {
    /// What to show on a tile.
    ///
    /// The title, falling back to the app id, falling back to something rather
    /// than an empty tile. A window with no title yet is normal — the title
    /// arrives in a later batch — and an unlabelled gap in the list looks like
    /// a rendering fault.
    pub fn label(&self) -> &str {
        if !self.title.trim().is_empty() {
            return &self.title;
        }
        if !self.app_id.trim().is_empty() {
            return &self.app_id;
        }
        "Untitled"
    }
}

/// The snapshot the UI reads.
pub type Shared = Arc<Mutex<Vec<Window>>>;

/// What a client may ask the compositor to do with a window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Activate,
    Close,
    Minimize,
    Unminimize,
}

/// Requests waiting to be sent, queued from the UI thread.
///
/// Queued rather than sent directly: the protocol objects live on the
/// connection's thread and are not `Send` in a way that makes calling them from
/// iced's thread reasonable. The UI pushes an id and an intent; the event thread
/// turns that into a protocol call.
///
/// # The queue is not enough on its own
///
/// The event thread waits on the Wayland socket. A queue the UI can push to
/// does nothing while that thread is **asleep**, and it is asleep almost always
/// — a desktop where nothing is happening produces no events. So a click was
/// queued and then sat there until the compositor next said something
/// unprompted, which might be seconds later or never.
///
/// Measured, not theorised: `examples/actprobe.rs` queued an `Activate`, waited
/// two seconds, and the window's `activated` state had not changed. The request
/// had never left the process.
///
/// So the outbox owns a pipe. Pushing writes a byte, the event thread polls the
/// socket and that pipe together, and a click is sent on the next turn of the
/// loop rather than on the next unrelated event.
pub struct Outbox {
    queue: Mutex<Vec<(u32, Request)>>,
    /// Write end of the wake-up pipe, absent when no event thread is listening.
    ///
    /// `Option` rather than an invalid descriptor: `OwnedFd` is a *live* fd by
    /// construction, and `from_raw_fd(-1)` asserts rather than building one. It
    /// is `#[track_caller]`, so the panic reads as a bug on the line that asked
    /// for the outbox. There is no in-band "no fd" value to reach for here.
    waker: Option<std::os::fd::OwnedFd>,
}

impl Outbox {
    /// An outbox nothing reads, for the strip that draws no window list.
    ///
    /// Its writes go nowhere and `push` already tolerates a failed one, so this
    /// is inert rather than a special case the callers have to know about.
    pub fn inert() -> Arc<Self> {
        Arc::new(Outbox {
            queue: Mutex::new(Vec::new()),
            waker: None,
        })
    }

    /// Queue a request and wake the event thread.
    pub fn push(&self, id: u32, request: Request) {
        if let Ok(mut held) = self.queue.lock() {
            held.push((id, request));
        }
        // One byte, and a failure is ignored deliberately. If the pipe is full
        // the thread is already awake with work pending, which is the state the
        // write was trying to produce. No waker means no thread to wake, and the
        // queue is drained by nobody — inert, as documented.
        if let Some(waker) = &self.waker {
            let _ = rustix::io::write(waker, &[1u8]);
        }
    }

    fn drain(&self) -> Vec<(u32, Request)> {
        match self.queue.lock() {
            Ok(mut held) => held.drain(..).collect(),
            Err(_) => Vec::new(),
        }
    }
}

/// Everything the event thread owns.
struct State {
    published: Shared,
    outbox: Arc<Outbox>,
    /// Live handles, by the id handed out to the UI.
    handles: HashMap<u32, ZwlrForeignToplevelHandleV1>,
    /// Accumulating state per handle, published on `done`.
    pending: HashMap<u32, Window>,
    /// Order windows first appeared.
    ///
    /// A taskbar that reorders itself as windows gain focus is unusable: the
    /// tile you are reaching for moves as you reach for it. First-seen order is
    /// stable and is what every taskbar worth using does.
    order: Vec<u32>,
    next_id: u32,
    /// Set when the manager reports `finished`, so the loop can stop rather
    /// than spin on a dead protocol object.
    finished: bool,
    /// Maps a protocol object back to the id handed to the UI.
    ///
    /// Handle events arrive on the object, and the UI only knows numbers. The
    /// alternative is scanning `handles` per event, which is a linear search on
    /// every title change.
    by_object: HashMap<wayland_client::backend::ObjectId, u32>,
    /// Needed by `activate`, which takes a seat.
    seat: Option<wayland_client::protocol::wl_seat::WlSeat>,
    /// Held for its lifetime: dropping the manager withdraws every handle.
    manager: Option<ZwlrForeignToplevelManagerV1>,
    /// Thumbnails of minimised windows, shared with the UI.
    thumbs: Thumbs,
    /// `hadal_stage_v1`, absent on any compositor that is not cusk.
    ///
    /// Absent is not an error and is not reported. The dock is expected to run
    /// on other compositors — it speaks nothing but standard protocols
    /// otherwise — and a taskbar that logs a complaint per session because a
    /// HadalOS extension is missing would be noise on every machine it is
    /// not on. Without it the stage strip shows labels and no pictures.
    stage: Option<HadalStageManagerV1>,
    /// Watch objects, one per window, held so they stay alive.
    ///
    /// Dropping one stops the thumbnails for that window arriving, which is
    /// the whole reason this is a map and not a series of discarded returns.
    watches: HashMap<u32, HadalStageThumbnailV1>,
}

impl State {
    /// Record or drop a thumbnail.
    ///
    /// Compared before assigning, for the same reason `publish` is: iced
    /// rebuilds its view whenever shared state changes, and writing an
    /// identical picture would redraw the dock forever for nothing. Here it
    /// matters more than in `publish` — the comparison is over a quarter
    /// megabyte, but the redraw it prevents is an image upload.
    fn set_thumb(&mut self, id: u32, thumbnail: Option<Thumbnail>) {
        let Ok(mut held) = self.thumbs.lock() else { return };
        match thumbnail {
            Some(mut fresh) => {
                match held.get(&id) {
                    // Identical pixels, so nothing is written and the
                    // revision does not move — which is what tells the UI its
                    // cached image handle is still good. A compositor that
                    // recaptures an unchanged window must not cost a texture
                    // upload.
                    Some(old) if old.pixels == fresh.pixels => return,
                    Some(old) => fresh.revision = old.revision.wrapping_add(1),
                    None => fresh.revision = 0,
                }
                held.insert(id, fresh);
            }
            None => {
                held.remove(&id);
            }
        }
    }

    /// Republish the snapshot in `order`.
    fn publish(&mut self) {
        let fresh: Vec<Window> = self
            .order
            .iter()
            .filter_map(|id| self.pending.get(id).cloned())
            .collect();
        if let Ok(mut held) = self.published.lock() {
            // Compared before assigning, for the reason the tray's poll is:
            // iced rebuilds its view whenever state changes, and writing an
            // identical list would redraw the dock forever for nothing.
            if *held != fresh {
                *held = fresh;
            }
        }
    }

    /// Send anything the UI has queued.
    fn drain_outbox(&mut self) {
        for (id, request) in self.outbox.drain() {
            // The window may have closed between the click and this loop.
            // Ordinary, not an error.
            let Some(handle) = self.handles.get(&id) else { continue };
            match request {
                Request::Activate => {
                    // `activate` needs a seat, and the protocol means "the seat
                    // this request came from". There is exactly one here; a
                    // multi-seat session would have to track which seat the
                    // click arrived on.
                    if let Some(seat) = &self.seat {
                        handle.activate(seat);
                    } else {
                        eprintln!("windows: no seat, cannot activate");
                    }
                }
                Request::Close => handle.close(),
                Request::Minimize => handle.set_minimized(),
                Request::Unminimize => handle.unset_minimized(),
            }
        }
    }
}

/// Start reading the window list, on its own thread.
///
/// Returns the snapshot and the outbox immediately; the snapshot fills in as
/// the compositor reports what exists. A compositor that does not implement the
/// protocol leaves it permanently empty and says so once — the dock still runs,
/// because a missing taskbar is better than no dock.
pub fn start() -> (Shared, Arc<Outbox>, Thumbs) {
    let published: Shared = Arc::new(Mutex::new(Vec::new()));
    let thumbs: Thumbs = Arc::new(Mutex::new(HashMap::new()));

    // Non-blocking on both ends: the writer must never stall the UI thread on a
    // full pipe, and the reader is drained opportunistically after a poll that
    // has already said there is something there.
    let (read_end, write_end) = match rustix::pipe::pipe_with(rustix::pipe::PipeFlags::NONBLOCK) {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("windows: no wake-up pipe, window list disabled: {e}");
            // A disabled list is better than a panic in a dock.
            return (published, Outbox::inert(), thumbs);
        }
    };
    let outbox = Arc::new(Outbox {
        queue: Mutex::new(Vec::new()),
        waker: Some(write_end),
    });
    let (thread_published, thread_outbox) = (published.clone(), outbox.clone());
    let thread_thumbs = thumbs.clone();

    std::thread::spawn(move || {
        let connection = match Connection::connect_to_env() {
            Ok(connection) => connection,
            Err(e) => {
                eprintln!("windows: no Wayland connection, window list disabled: {e}");
                return;
            }
        };

        let display = connection.display();
        let mut queue = connection.new_event_queue();
        let handle = queue.handle();
        display.get_registry(&handle, ());

        let mut state = State {
            published: thread_published,
            outbox: thread_outbox,
            handles: HashMap::new(),
            pending: HashMap::new(),
            order: Vec::new(),
            next_id: 1,
            finished: false,
            by_object: HashMap::new(),
            seat: None,
            manager: None,
            thumbs: thread_thumbs,
            stage: None,
            watches: HashMap::new(),
        };

        // One round trip to learn what globals exist, and a second so the
        // manager's initial burst of `toplevel` events has arrived before the
        // first draw. Without the second, the dock's first frame shows an empty
        // taskbar on a session full of windows, and fills in a moment later.
        if queue.roundtrip(&mut state).is_err() {
            eprintln!("windows: registry roundtrip failed; window list disabled");
            return;
        }
        if state.manager.is_none() {
            eprintln!(
                "windows: the compositor does not offer \
                 zwlr_foreign_toplevel_management_v1; the window list will stay empty"
            );
            return;
        }
        let _ = queue.roundtrip(&mut state);
        state.publish();

        while !state.finished {
            state.drain_outbox();

            // Everything queued above has to reach the socket before this
            // thread goes back to sleep, or it sleeps holding the very requests
            // it just took off the queue.
            if connection.flush().is_err() {
                eprintln!("windows: connection lost while flushing; window list stopped");
                return;
            }

            // Any events already buffered are handled before waiting, or a
            // batch that arrived during `drain_outbox` would sit unprocessed
            // until the *next* one showed up.
            if queue.dispatch_pending(&mut state).is_err() {
                eprintln!("windows: connection lost; window list stopped");
                return;
            }

            // `prepare_read` returning `None` means more events were queued in
            // the meantime — go round again rather than waiting on a socket
            // whose data is already in hand.
            let Some(guard) = connection.prepare_read() else { continue };

            // Wait on the socket *and* the wake-up pipe. Waiting on the socket
            // alone is what left queued requests unsent: this thread would
            // sleep until the compositor spoke, which on an idle desktop is
            // never.
            // Bound, not inlined: `connection_fd` returns a borrowed fd whose
            // temporary would be dropped before `poll` ran.
            let socket = guard.connection_fd();
            let mut fds = [
                rustix::event::PollFd::new(&socket, rustix::event::PollFlags::IN),
                rustix::event::PollFd::new(&read_end, rustix::event::PollFlags::IN),
            ];
            if rustix::event::poll(&mut fds, None).is_err() {
                eprintln!("windows: poll failed; window list stopped");
                return;
            }

            let woken = fds[1].revents().contains(rustix::event::PollFlags::IN);
            if woken {
                // Drained so the pipe does not stay readable and spin the loop.
                // A short buffer is enough: the bytes are a signal, not data,
                // and any number of them means the same thing.
                let mut sink = [0u8; 64];
                while rustix::io::read(&read_end, &mut sink).is_ok_and(|n| n == sink.len()) {}
            }

            if fds[0].revents().contains(rustix::event::PollFlags::IN) {
                if guard.read().is_err() {
                    eprintln!("windows: connection lost; window list stopped");
                    return;
                }
            } else {
                // Woken only by the pipe, so there is nothing to read from the
                // socket. The guard has to be released without reading, or the
                // next `prepare_read` deadlocks against it.
                drop(guard);
            }
        }
    });

    (published, outbox, thumbs)
}

impl Dispatch<wl_registry::WlRegistry, ()> for State {
    fn event(
        state: &mut Self,
        registry: &wl_registry::WlRegistry,
        event: wl_registry::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        let wl_registry::Event::Global { name, interface, version } = event else {
            return;
        };
        match interface.as_str() {
            "zwlr_foreign_toplevel_manager_v1" => {
                // Bound at whatever both sides support. cusk advertises 3;
                // asking for more than the compositor has is a protocol error
                // that kills the connection, and asking for less than we can
                // use loses `parent` and the newer state bits.
                let wanted = version.min(3);
                state.manager = Some(
                    registry.bind::<ZwlrForeignToplevelManagerV1, _, _>(name, wanted, handle, ()),
                );
            }
            "hadal_stage_manager_v1" => {
                state.stage = Some(registry.bind::<HadalStageManagerV1, _, _>(
                    name,
                    version.min(1),
                    handle,
                    (),
                ));
            }
            // Needed for `activate`, which takes a seat. Without it the list is
            // readable and every tile is inert.
            "wl_seat" => {
                state.seat = Some(registry.bind::<wayland_client::protocol::wl_seat::WlSeat, _, _>(
                    name,
                    version.min(7),
                    handle,
                    (),
                ));
            }
            _ => {}
        }
    }
}

impl Dispatch<wayland_client::protocol::wl_seat::WlSeat, ()> for State {
    fn event(
        _: &mut Self,
        _: &wayland_client::protocol::wl_seat::WlSeat,
        _: wayland_client::protocol::wl_seat::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        // The seat is bound for `activate` and nothing else. Capabilities and
        // names are the business of whoever handles input, which is iced.
    }
}

impl Dispatch<ZwlrForeignToplevelManagerV1, ()> for State {
    fn event(
        state: &mut Self,
        _: &ZwlrForeignToplevelManagerV1,
        event: zwlr_foreign_toplevel_manager_v1::Event,
        _: &(),
        _: &Connection,
        handle: &QueueHandle<Self>,
    ) {
        match event {
            zwlr_foreign_toplevel_manager_v1::Event::Toplevel { toplevel } => {
                let id = state.next_id;
                state.next_id += 1;
                // Recorded with empty strings and no states. Everything real
                // arrives as deltas and is published on `done`; a window that
                // appeared but has not described itself yet is still a window,
                // and leaving it out until it has a title makes new windows
                // seem to appear late.
                state.pending.insert(
                    id,
                    Window {
                        id,
                        title: String::new(),
                        app_id: String::new(),
                        activated: false,
                        minimized: false,
                        maximized: false,
                        fullscreen: false,
                    },
                );
                state.order.push(id);
                state.handles.insert(id, toplevel.clone());
                // The id is stored on the handle's user data so its own events
                // can find their way back to the right record.
                state.by_object.insert(toplevel.id(), id);
                // Asked for at once, not when the window is first minimised.
                // A `watch` sent on minimise would race the compositor's
                // capture and lose the first picture roughly half the time,
                // and "the thumbnail appears the second time you minimise it"
                // is a bug nobody would report clearly.
                if let Some(stage) = &state.stage {
                    state
                        .watches
                        .insert(id, stage.watch(&toplevel, handle, id));
                }
            }
            zwlr_foreign_toplevel_manager_v1::Event::Finished => {
                // The compositor has withdrawn the protocol. Nothing more will
                // arrive, so the list is emptied rather than left showing
                // windows that can no longer be acted on.
                state.finished = true;
                state.order.clear();
                state.pending.clear();
                state.handles.clear();
                state.publish();
            }
            _ => {}
        }
    }

    // Without this, the first `toplevel` event **panics inside
    // wayland-client**: "Missing event_created_child specialization for event
    // opcode 0". The event carries a *new object*, and the generated code has
    // no way to know which `Dispatch` impl should own it — the mapping has to be
    // declared, and there is no compile-time check that it was.
    //
    // Found by running `examples/probe.rs` against a real compositor with a real
    // window. Nothing in the unit tests could have caught it: they exercise the
    // decoding, and this is the plumbing that delivers anything to decode.
    wayland_client::event_created_child!(State, ZwlrForeignToplevelManagerV1, [
        zwlr_foreign_toplevel_manager_v1::EVT_TOPLEVEL_OPCODE => (ZwlrForeignToplevelHandleV1, ())
    ]);
}

impl Dispatch<ZwlrForeignToplevelHandleV1, ()> for State {
    fn event(
        state: &mut Self,
        handle: &ZwlrForeignToplevelHandleV1,
        event: zwlr_foreign_toplevel_handle_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        let Some(&id) = state.by_object.get(&handle.id()) else { return };

        match event {
            zwlr_foreign_toplevel_handle_v1::Event::Title { title } => {
                if let Some(window) = state.pending.get_mut(&id) {
                    window.title = title;
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::AppId { app_id } => {
                if let Some(window) = state.pending.get_mut(&id) {
                    window.app_id = app_id;
                }
            }
            zwlr_foreign_toplevel_handle_v1::Event::State { state: states } => {
                // The array is the *complete* state, not a delta — so every
                // flag is recomputed rather than or-ed in. Or-ing would mean a
                // window that was maximised once stayed maximised in the dock
                // forever.
                let flags = decode_states(&states);
                if let Some(window) = state.pending.get_mut(&id) {
                    window.activated = flags.activated;
                    window.minimized = flags.minimized;
                    window.maximized = flags.maximized;
                    window.fullscreen = flags.fullscreen;
                }
            }
            // "That batch is complete." Everything above only mutates the
            // pending record; this is what makes it visible, which is why a
            // half-described window never reaches the screen.
            zwlr_foreign_toplevel_handle_v1::Event::Done => state.publish(),
            zwlr_foreign_toplevel_handle_v1::Event::Closed => {
                handle.destroy();
                state.by_object.remove(&handle.id());
                state.handles.remove(&id);
                state.pending.remove(&id);
                state.order.retain(|held| *held != id);
                state.publish();
            }
            _ => {}
        }
    }
}

/// The state flags a `state` event carries.
struct Flags {
    activated: bool,
    minimized: bool,
    maximized: bool,
    fullscreen: bool,
}

/// Decode the protocol's state array.
///
/// The values are a `uint` array on the wire, so this is four bytes per entry
/// in native order. Kept as a free function over the raw bytes so it can be
/// tested without a compositor — the same reason `cusk::toplevel` is pure.
fn decode_states(bytes: &[u8]) -> Flags {
    let mut flags = Flags {
        activated: false,
        minimized: false,
        maximized: false,
        fullscreen: false,
    };
    for chunk in bytes.chunks_exact(4) {
        let value = u32::from_ne_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        // The protocol's own enum. Matched by value rather than through the
        // generated type because the array is untyped bytes.
        match value {
            0 => flags.maximized = true,
            1 => flags.minimized = true,
            2 => flags.activated = true,
            3 => flags.fullscreen = true,
            // A state this version does not know. Ignored rather than treated
            // as an error: the protocol is allowed to grow, and a dock that
            // disconnected over an unfamiliar state would break on upgrade.
            _ => {}
        }
    }
    flags
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(states: &[u32]) -> Vec<u8> {
        states.iter().flat_map(|s| s.to_ne_bytes()).collect()
    }

    /// The right-hand strip builds an inert outbox on every start, so a panic
    /// here is not a missing taskbar — it is the whole dock gone before it maps
    /// a surface, with `cusk` reporting only that it spawned a pid. It was:
    /// `inert` held its waker as `OwnedFd::from_raw_fd(-1)`, and that asserts.
    #[test]
    fn an_inert_outbox_builds_and_swallows_a_push() {
        let outbox = Outbox::inert();
        outbox.push(1, Request::Activate);
        assert_eq!(
            outbox.drain(),
            vec![(1, Request::Activate)],
            "the queue still records, it is only the wake-up that goes nowhere"
        );
    }

    #[test]
    fn states_decode_from_the_wire_format() {
        let flags = decode_states(&encode(&[2]));
        assert!(flags.activated);
        assert!(!flags.minimized && !flags.maximized && !flags.fullscreen);

        let both = decode_states(&encode(&[0, 2]));
        assert!(both.maximized && both.activated);
    }

    /// The array is the complete state, so an empty one means *nothing* is set.
    /// Treating empty as "no change" would leave a window activated forever
    /// after it lost focus.
    #[test]
    fn an_empty_state_array_clears_everything() {
        let flags = decode_states(&[]);
        assert!(!flags.activated && !flags.minimized);
    }

    /// The protocol may grow. A dock that treated an unknown state as an error
    /// would break the first time the compositor was upgraded.
    #[test]
    fn unknown_states_are_ignored() {
        let flags = decode_states(&encode(&[2, 99]));
        assert!(flags.activated, "the known state still applies");
    }

    /// A truncated array must not panic or read past the end — this is another
    /// process's data.
    #[test]
    fn a_truncated_state_array_is_survivable() {
        let flags = decode_states(&[0, 0]);
        assert!(!flags.activated && !flags.maximized);
    }

    /// A window with no title yet is normal: the title arrives in a later
    /// batch. An unlabelled gap in the taskbar reads as a rendering fault.
    #[test]
    fn a_label_always_has_something_to_draw() {
        let mut window = Window {
            id: 1,
            title: String::new(),
            app_id: String::new(),
            activated: false,
            minimized: false,
            maximized: false,
            fullscreen: false,
        };
        assert_eq!(window.label(), "Untitled");

        window.app_id = "org.kde.konsole".into();
        assert_eq!(window.label(), "org.kde.konsole", "the app id is the fallback");

        window.title = "zsh".into();
        assert_eq!(window.label(), "zsh", "the title wins once it arrives");

        window.title = "   ".into();
        assert_eq!(window.label(), "org.kde.konsole", "blank is not a title");
    }
}

/// The stage manager says nothing; it exists to create watch objects.
impl Dispatch<HadalStageManagerV1, ()> for State {
    fn event(
        _: &mut Self,
        _: &HadalStageManagerV1,
        _: <HadalStageManagerV1 as Proxy>::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
    }
}

impl Dispatch<HadalStageThumbnailV1, u32> for State {
    fn event(
        state: &mut Self,
        _: &HadalStageThumbnailV1,
        event: hadal_stage_thumbnail_v1::Event,
        id: &u32,
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        match event {
            hadal_stage_thumbnail_v1::Event::Image {
                fd,
                width,
                height,
                stride,
                format,
            } => {
                // Only the one format exists at version 1, and an unknown one
                // is skipped rather than guessed at. `WEnum::Unknown` is what
                // a future compositor sending a format this build has never
                // heard of looks like, and drawing those bytes as RGBA would
                // put colour noise on the strip.
                if !matches!(
                    format.into_result(),
                    Ok(hadal_stage_thumbnail_v1::Format::Abgr8888)
                ) {
                    return;
                }
                // The descriptor is ours from here. `OwnedFd` closes it when
                // this scope ends, which is the part a taskbar gets wrong:
                // one leaked per minimise exhausts the process in an
                // afternoon of ordinary use.
                let Some(thumbnail) =
                    crate::stage::read(std::os::fd::AsFd::as_fd(&fd), width, height, stride)
                else {
                    return;
                };
                state.set_thumb(*id, Some(thumbnail));
            }
            hadal_stage_thumbnail_v1::Event::Cleared => state.set_thumb(*id, None),
        }
    }
}
