//! `zwlr_foreign_toplevel_management_v1`, server side.
//!
//! The Wayland half of what `cusk::toplevel` models. That module decides *what*
//! to send; this one owns the objects and puts it on the wire. The split is the
//! same one `panel.rs` makes for geometry, and for the same reason: the part
//! with the interesting logic should be testable without a compositor running.
//!
//! Structured the way smithay structures its own protocol implementations —
//! a state struct, a handler trait the compositor implements, dispatch generic
//! over `D`, and a delegate macro — so that adding it to `Cusk` looks like
//! adding any other protocol rather than like a special case.
//!
//! **No new dependency.** smithay re-exports `wayland_protocols_wlr` (it needs
//! it for layer-shell), and that crate already generates this protocol's
//! bindings. `use smithay::reexports::wayland_protocols_wlr` and there is
//! nothing to add to Cargo.toml — which for a project whose stated goal is as
//! few dependencies as possible is worth a sentence.
//!
//! # One handle per client, one diff for everybody
//!
//! The protocol creates a `zwlr_foreign_toplevel_handle_v1` per bound manager,
//! so two docks each get their own object for the same window. They are told
//! identical things, so the diff is computed once against the last snapshot and
//! replayed to every handle. Tracking a snapshot per client would be correct
//! too and would differ only in cost, but it would also make "what did we tell
//! them" a set of answers instead of one, and the first bug in this protocol is
//! always a client that thinks a window is still focused.

use std::collections::HashMap;

use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::{
    zwlr_foreign_toplevel_handle_v1::{self, ZwlrForeignToplevelHandleV1},
    zwlr_foreign_toplevel_manager_v1::{self, ZwlrForeignToplevelManagerV1},
};
use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::toplevel::{diff, Event, Snapshot};

/// Which window a handle refers to.
///
/// An opaque integer rather than a `Window`: this module has no business
/// holding compositor types, and the compositor is free to key its own side
/// however it likes as long as the ids are stable for a window's lifetime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ToplevelId(pub u64);

/// What a client asked to be done to a window.
///
/// The compositor decides whether to honour it. Nothing here focuses or closes
/// anything — a protocol module that knew how to raise a window would need to
/// know about workspaces, tiling and the seat, and would stop being testable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    Activate,
    Close,
    SetMinimized(bool),
    SetMaximized(bool),
    SetFullscreen(bool),
}

pub trait ForeignToplevelHandler {
    fn foreign_toplevel_state(&mut self) -> &mut ForeignToplevelState;
    /// Act on a client request. Returning without doing anything is allowed and
    /// is what an unimplemented action should do — the protocol has no failure
    /// reply, so a compositor that cannot minimise simply does not.
    fn request(&mut self, id: ToplevelId, request: Request);
}

#[derive(Debug)]
struct Tracked {
    /// What every client has already been told. `None` means the window exists
    /// but no snapshot has been published yet.
    last: Option<Snapshot>,
    handles: Vec<ZwlrForeignToplevelHandleV1>,
}

#[derive(Debug, Default)]
pub struct ForeignToplevelState {
    managers: Vec<ZwlrForeignToplevelManagerV1>,
    toplevels: HashMap<ToplevelId, Tracked>,
    /// Insertion order, so a newly-bound manager is told about windows in the
    /// order they appeared rather than in HashMap order. A dock that renders in
    /// the order it was told would otherwise shuffle its entries on restart.
    order: Vec<ToplevelId>,
}

impl ForeignToplevelState {
    /// Advertise the global. Version 3 is what the protocol is at, and is what
    /// every current dock expects.
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<ZwlrForeignToplevelManagerV1, ()> + 'static,
    {
        display.create_global::<D, ZwlrForeignToplevelManagerV1, _>(3, ());
        Self::default()
    }

    /// A window has appeared. It is announced to every bound manager, but
    /// nothing is said about it until the first `publish`.
    pub fn add<D>(&mut self, display: &DisplayHandle, id: ToplevelId)
    where
        D: Dispatch<ZwlrForeignToplevelHandleV1, ToplevelId> + 'static,
    {
        if self.toplevels.contains_key(&id) {
            return;
        }
        let mut handles = Vec::new();
        for manager in &self.managers {
            if let Some(handle) = announce::<D>(display, manager, id) {
                handles.push(handle);
            }
        }
        self.toplevels.insert(id, Tracked { last: None, handles });
        self.order.push(id);
    }

    /// Publish the current state of a window, sending only what changed.
    ///
    /// Safe and cheap to call on every commit: `toplevel::diff` returns nothing
    /// when nothing changed, and this then touches no sockets at all.
    pub fn publish(&mut self, id: ToplevelId, snapshot: &Snapshot) {
        let Some(tracked) = self.toplevels.get_mut(&id) else {
            return;
        };
        let events = diff(tracked.last.as_ref(), snapshot);
        if events.is_empty() {
            return;
        }
        for handle in &tracked.handles {
            send(handle, &events);
        }
        tracked.last = Some(snapshot.clone());
    }

    /// A window has gone. Sends `closed` and forgets it.
    ///
    /// The handle objects are not destroyed here — the protocol makes that the
    /// client's job after it receives `closed`, and destroying them from this
    /// side would race a client that is still reading.
    pub fn remove(&mut self, id: ToplevelId) {
        let Some(tracked) = self.toplevels.remove(&id) else {
            return;
        };
        self.order.retain(|other| *other != id);
        for handle in &tracked.handles {
            handle.closed();
        }
    }

    pub fn contains(&self, id: ToplevelId) -> bool {
        self.toplevels.contains_key(&id)
    }
}

/// Create a handle for `id` on `manager`'s client and announce it.
fn announce<D>(
    display: &DisplayHandle,
    manager: &ZwlrForeignToplevelManagerV1,
    id: ToplevelId,
) -> Option<ZwlrForeignToplevelHandleV1>
where
    D: Dispatch<ZwlrForeignToplevelHandleV1, ToplevelId> + 'static,
{
    let client = manager.client()?;
    let handle = client
        .create_resource::<ZwlrForeignToplevelHandleV1, _, D>(display, manager.version(), id)
        .ok()?;
    manager.toplevel(&handle);
    Some(handle)
}

/// Put one burst on the wire.
fn send(handle: &ZwlrForeignToplevelHandleV1, events: &[Event]) {
    for event in events {
        match event {
            Event::Title(title) => handle.title(title.clone()),
            Event::AppId(app_id) => handle.app_id(app_id.clone()),
            Event::OutputEnter(_) | Event::OutputLeave(_) => {
                // Deliberately not sent yet. These carry a `wl_output`, and
                // cusk advertises exactly one output today (main.rs holds a
                // single `Output`), so there is nothing a client could learn
                // from them. Sending a wrong output is worse than sending
                // none: a dock that groups by output would group by a lie.
                //
                // `toplevel::diff` already produces them and is tested on
                // them, so this becomes a lookup rather than new logic when
                // multi-output lands.
            }
            Event::State(states) => {
                handle.state(states.iter().map(|s| *s as u32).flat_map(u32::to_ne_bytes).collect())
            }
            Event::Done => handle.done(),
        }
    }
}

// ── dispatch ───────────────────────────────────────────────────────────

impl<D> GlobalDispatch<ZwlrForeignToplevelManagerV1, (), D> for ForeignToplevelState
where
    D: GlobalDispatch<ZwlrForeignToplevelManagerV1, ()>
        + Dispatch<ZwlrForeignToplevelManagerV1, ()>
        + Dispatch<ZwlrForeignToplevelHandleV1, ToplevelId>
        + ForeignToplevelHandler
        + 'static,
{
    fn bind(
        state: &mut D,
        display: &DisplayHandle,
        _client: &Client,
        resource: New<ZwlrForeignToplevelManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        let manager = data_init.init(resource, ());

        // A manager binding late must be told about the windows that already
        // exist, in the order they appeared, and then brought up to date. A
        // dock started after the session would otherwise show an empty
        // taskbar until something happened to change.
        let existing: Vec<(ToplevelId, Option<Snapshot>)> = {
            let this = state.foreign_toplevel_state();
            this.order
                .iter()
                .filter_map(|id| this.toplevels.get(id).map(|t| (*id, t.last.clone())))
                .collect()
        };

        for (id, last) in existing {
            let Some(handle) = announce::<D>(display, &manager, id) else {
                continue;
            };
            if let Some(snapshot) = last {
                send(&handle, &diff(None, &snapshot));
            }
            if let Some(tracked) = state.foreign_toplevel_state().toplevels.get_mut(&id) {
                tracked.handles.push(handle);
            }
        }

        state.foreign_toplevel_state().managers.push(manager);
    }
}

impl<D> Dispatch<ZwlrForeignToplevelManagerV1, (), D> for ForeignToplevelState
where
    D: Dispatch<ZwlrForeignToplevelManagerV1, ()> + ForeignToplevelHandler + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        resource: &ZwlrForeignToplevelManagerV1,
        request: zwlr_foreign_toplevel_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        if let zwlr_foreign_toplevel_manager_v1::Request::Stop = request {
            // `finished` is a promise that no further events will arrive, so
            // the manager must be dropped before it is sent — otherwise a
            // window closing afterwards would send `toplevel` on an object the
            // client has already stopped listening to.
            let this = state.foreign_toplevel_state();
            this.managers.retain(|m| m != resource);
            resource.finished();
        }
    }

    fn destroyed(
        state: &mut D,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &ZwlrForeignToplevelManagerV1,
        _data: &(),
    ) {
        state.foreign_toplevel_state().managers.retain(|m| m != resource);
    }
}

impl<D> Dispatch<ZwlrForeignToplevelHandleV1, ToplevelId, D> for ForeignToplevelState
where
    D: Dispatch<ZwlrForeignToplevelHandleV1, ToplevelId> + ForeignToplevelHandler + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &ZwlrForeignToplevelHandleV1,
        request: zwlr_foreign_toplevel_handle_v1::Request,
        id: &ToplevelId,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        use zwlr_foreign_toplevel_handle_v1::Request as R;
        let action = match request {
            R::Activate { .. } => Some(Request::Activate),
            R::Close => Some(Request::Close),
            R::SetMinimized => Some(Request::SetMinimized(true)),
            R::UnsetMinimized => Some(Request::SetMinimized(false)),
            R::SetMaximized => Some(Request::SetMaximized(true)),
            R::UnsetMaximized => Some(Request::SetMaximized(false)),
            R::SetFullscreen { .. } => Some(Request::SetFullscreen(true)),
            R::UnsetFullscreen => Some(Request::SetFullscreen(false)),
            // A hint for minimise animations. cusk has no animations, and the
            // protocol explicitly allows ignoring it.
            R::SetRectangle { .. } => None,
            R::Destroy => None,
            _ => None,
        };
        if let Some(action) = action {
            state.request(*id, action);
        }
    }

    fn destroyed(
        state: &mut D,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &ZwlrForeignToplevelHandleV1,
        id: &ToplevelId,
    ) {
        if let Some(tracked) = state.foreign_toplevel_state().toplevels.get_mut(id) {
            tracked.handles.retain(|h| h != resource);
        }
    }
}

/// Wire this protocol into a compositor state type.
///
/// Spelled with full paths rather than local aliases: a `pub use` of a name
/// this module merely imported is not re-exportable, and the macro expands in
/// the caller's crate where the short names do not exist anyway.
#[macro_export]
macro_rules! delegate_foreign_toplevel {
    ($ty:ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1: ()
        ] => $crate::foreign_toplevel::ForeignToplevelState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_manager_v1::ZwlrForeignToplevelManagerV1: ()
        ] => $crate::foreign_toplevel::ForeignToplevelState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::zwlr_foreign_toplevel_handle_v1::ZwlrForeignToplevelHandleV1: $crate::foreign_toplevel::ToplevelId
        ] => $crate::foreign_toplevel::ForeignToplevelState);
    };
}

#[cfg(test)]
mod tests {
    use crate::toplevel::State;

    /// The state array goes on the wire as a `uint` array, so the encoding is
    /// native-endian bytes of each enum value. Getting this wrong shows up as a
    /// dock that believes every window is maximised.
    #[test]
    fn states_encode_as_native_endian_u32s() {
        let states = [State::Maximized, State::Activated];
        let bytes: Vec<u8> = states
            .iter()
            .map(|s| *s as u32)
            .flat_map(u32::to_ne_bytes)
            .collect();
        assert_eq!(bytes.len(), 8);
        assert_eq!(&bytes[0..4], &0u32.to_ne_bytes());
        assert_eq!(&bytes[4..8], &2u32.to_ne_bytes());
    }

    #[test]
    fn an_empty_state_set_encodes_to_an_empty_array() {
        let bytes: Vec<u8> = Vec::<State>::new()
            .iter()
            .map(|s| *s as u32)
            .flat_map(u32::to_ne_bytes)
            .collect();
        assert!(bytes.is_empty());
    }
}
