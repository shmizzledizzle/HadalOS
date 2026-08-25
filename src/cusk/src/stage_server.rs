//! The Wayland server side of `stage` — objects, globals and dispatch.
//!
//! Same split as `toplevel`/`foreign_toplevel`: `stage` decides what a
//! thumbnail *is* and can be tested without a compositor; this puts one on the
//! wire and cannot be tested without one. Structured the way smithay
//! structures its own protocol implementations — a state struct, a handler
//! trait, dispatch generic over `D`, a delegate macro — so adding it to `Cusk`
//! looks like adding any other protocol.
//!
//! # Who owns the pictures
//!
//! Not this module. `Stage` lives on the compositor, and this asks for a
//! snapshot through the handler trait rather than keeping a second copy.
//!
//! That is a deliberate refusal of the obvious design. A `StageState` holding
//! its own `HashMap<ToplevelId, Snapshot>` would need the compositor to keep
//! the two in step on every capture, forget and close, and the first time they
//! disagreed the symptom would be a dock showing a picture of a window that is
//! on screen — with nothing in either data structure to say which one was
//! wrong. There is one store, and this module has a view of it.
//!
//! # One object per watcher, one memfd per publish
//!
//! `watch` hands out an object per client per window, so two docks watching
//! the same window get their own. They are told the same things at the same
//! time, and — unlike `foreign_toplevel`, where each handle gets its own
//! events assembled — they are told it out of one shared memfd, sealed before
//! anybody sees it. Sharing the memory is safe precisely because it is sealed:
//! no client can alter a picture another client is reading.

use std::collections::HashMap;
use std::os::fd::{AsFd, OwnedFd};

use smithay::reexports::wayland_server::{
    Client, DataInit, Dispatch, DisplayHandle, GlobalDispatch, New, Resource,
};

use crate::foreign_toplevel::ToplevelId;
use crate::stage::Snapshot;
use crate::stage_protocol::{
    hadal_stage_manager_v1::{self, HadalStageManagerV1},
    hadal_stage_thumbnail_v1::{self, HadalStageThumbnailV1},
};

pub trait StageHandler {
    fn stage_state(&mut self) -> &mut StageState;
    /// The current picture of a window, if there is one.
    ///
    /// Asked when a client starts watching, so that a dock which binds after a
    /// window was minimised is not left with an empty tile until the next
    /// time something changes. The compositor answers from its `Stage`; there
    /// is no second copy here to go stale.
    fn thumbnail(&self, id: ToplevelId) -> Option<&Snapshot>;
}

#[derive(Debug, Default)]
pub struct StageState {
    /// Live watchers, by window. A window with no watchers has no entry, so
    /// this is empty on a desktop with no taskbar rather than a map of empty
    /// vectors.
    watchers: HashMap<ToplevelId, Vec<HadalStageThumbnailV1>>,
}

impl StageState {
    /// Advertise the global.
    pub fn new<D>(display: &DisplayHandle) -> Self
    where
        D: GlobalDispatch<HadalStageManagerV1, ()> + 'static,
    {
        display.create_global::<D, HadalStageManagerV1, _>(1, ());
        Self::default()
    }

    /// There is a new picture of this window. Send it to everyone watching.
    ///
    /// Costs nothing when nobody is: the memfd is created after the watcher
    /// list is found to be non-empty, so a session with no dock running does
    /// not allocate and seal a file per minimise.
    pub fn publish(&mut self, id: ToplevelId, snapshot: &Snapshot) {
        let Some(watchers) = self.watchers.get(&id) else { return };
        if watchers.is_empty() {
            return;
        }
        let Some(fd) = seal(snapshot) else {
            // Already logged by `seal`. Saying nothing is the correct
            // fallback: a client that is told nothing keeps showing no
            // picture, which is what it was showing a moment ago.
            return;
        };
        for watcher in watchers {
            send_image(watcher, &fd, snapshot);
        }
    }

    /// There is no longer a picture of this window.
    ///
    /// Called when it is restored and when it closes. Both are "stop drawing
    /// what you have" and neither is "this object is now invalid" — see the
    /// protocol description for why the object outlives the window.
    pub fn clear(&mut self, id: ToplevelId) {
        let Some(watchers) = self.watchers.get(&id) else { return };
        for watcher in watchers {
            watcher.cleared();
        }
    }

    /// How many windows are being watched. For tests and for `stageprobe`.
    pub fn watched(&self) -> usize {
        self.watchers.len()
    }
}

/// Put a snapshot in a memfd nobody can change.
///
/// Sealed against growing, shrinking and writing before it leaves this
/// function, which is what lets one file be handed to every watcher: a client
/// cannot corrupt a picture another client has mapped, and cannot truncate it
/// into a mapping that faults on read.
///
/// Sealing needs the fd to have no writable *mappings*; an open writable
/// descriptor is fine, which is why the bytes can be written here and sealed
/// immediately after.
fn seal(snapshot: &Snapshot) -> Option<OwnedFd> {
    use smithay::reexports::rustix::fs::{fcntl_add_seals, memfd_create, MemfdFlags, SealFlags};
    use smithay::reexports::rustix::io::write;

    // Refused rather than sent. A snapshot whose length disagrees with its
    // dimensions would become a client mapping `height * stride` bytes of a
    // shorter file — a SIGBUS in the dock, reported against the dock.
    if !snapshot.is_consistent() {
        tracing::warn!(
            "refusing to publish an inconsistent thumbnail: {}x{} with {} bytes",
            snapshot.width,
            snapshot.height,
            snapshot.pixels.len()
        );
        return None;
    }

    let fd = memfd_create("cusk-stage", MemfdFlags::CLOEXEC | MemfdFlags::ALLOW_SEALING)
        .inspect_err(|err| tracing::warn!("no memfd for a thumbnail: {err}"))
        .ok()?;

    // write(2) is allowed to write less than it was given, and a short write
    // here is a picture with a torn bottom rather than an error.
    let mut written = 0;
    while written < snapshot.pixels.len() {
        match write(&fd, &snapshot.pixels[written..]) {
            Ok(0) => {
                tracing::warn!("thumbnail write stalled at {written} bytes");
                return None;
            }
            Ok(n) => written += n,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => {
                tracing::warn!("could not write a thumbnail: {err}");
                return None;
            }
        }
    }

    fcntl_add_seals(
        &fd,
        SealFlags::SHRINK | SealFlags::GROW | SealFlags::WRITE | SealFlags::SEAL,
    )
    .inspect_err(|err| tracing::warn!("could not seal a thumbnail: {err}"))
    .ok()?;

    Some(fd)
}

fn send_image(watcher: &HadalStageThumbnailV1, fd: &OwnedFd, snapshot: &Snapshot) {
    watcher.image(
        fd.as_fd(),
        snapshot.width,
        snapshot.height,
        snapshot.width * 4,
        hadal_stage_thumbnail_v1::Format::Abgr8888,
    );
}

impl<D> GlobalDispatch<HadalStageManagerV1, (), D> for StageState
where
    D: GlobalDispatch<HadalStageManagerV1, ()>
        + Dispatch<HadalStageManagerV1, ()>
        + StageHandler
        + 'static,
{
    fn bind(
        _state: &mut D,
        _display: &DisplayHandle,
        _client: &Client,
        resource: New<HadalStageManagerV1>,
        _global_data: &(),
        data_init: &mut DataInit<'_, D>,
    ) {
        // Nothing is recorded about the manager. It exists only to create
        // thumbnail objects, and unlike `foreign_toplevel`'s manager it is
        // never sent anything, so a list of them would be a list nothing reads.
        data_init.init(resource, ());
    }
}

impl<D> Dispatch<HadalStageManagerV1, (), D> for StageState
where
    D: Dispatch<HadalStageManagerV1, ()>
        + Dispatch<HadalStageThumbnailV1, ToplevelId>
        + StageHandler
        + 'static,
{
    fn request(
        state: &mut D,
        _client: &Client,
        _resource: &HadalStageManagerV1,
        request: hadal_stage_manager_v1::Request,
        _data: &(),
        _display: &DisplayHandle,
        data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            hadal_stage_manager_v1::Request::Watch {
                thumbnail,
                toplevel,
            } => {
                // The id lives on the foreign-toplevel handle's user data,
                // put there by `foreign_toplevel::announce`. Reading it here
                // rather than keeping a second object-to-id map is the reason
                // `watch` takes an object at all.
                //
                // `None` means a handle this compositor did not create, which
                // the protocol cannot express as an error at version 1. The
                // object is still initialised — refusing to would leave the
                // client with a new_id it can never destroy — and simply never
                // hears anything.
                let Some(&id) = toplevel.data::<ToplevelId>() else {
                    tracing::warn!("stage watch for a toplevel handle with no id");
                    data_init.init(thumbnail, ToplevelId(u64::MAX));
                    return;
                };

                let watcher = data_init.init(thumbnail, id);

                // Answered before the object is recorded, so that a client
                // watching an already-minimised window gets its picture now
                // instead of at the next capture — which for a window that is
                // already hidden may be never.
                if let Some(snapshot) = state.thumbnail(id) {
                    // Cloned out because `seal` and `send_image` need the
                    // snapshot while `stage_state` needs the borrow. A
                    // thumbnail is at most 256 kB and this happens once per
                    // watch, not per frame.
                    let snapshot = snapshot.clone();
                    if let Some(fd) = seal(&snapshot) {
                        send_image(&watcher, &fd, &snapshot);
                    }
                }

                state.stage_state().watchers.entry(id).or_default().push(watcher);
            }
            // Destructors arrive here as well as freeing the object; there is
            // nothing to do beyond what the generated code already did.
            hadal_stage_manager_v1::Request::Destroy => {}
        }
    }
}

impl<D> Dispatch<HadalStageThumbnailV1, ToplevelId, D> for StageState
where
    D: Dispatch<HadalStageThumbnailV1, ToplevelId> + StageHandler + 'static,
{
    fn request(
        _state: &mut D,
        _client: &Client,
        _resource: &HadalStageThumbnailV1,
        request: hadal_stage_thumbnail_v1::Request,
        _data: &ToplevelId,
        _display: &DisplayHandle,
        _data_init: &mut DataInit<'_, D>,
    ) {
        match request {
            hadal_stage_thumbnail_v1::Request::Destroy => {}
        }
    }

    fn destroyed(
        state: &mut D,
        _client: smithay::reexports::wayland_server::backend::ClientId,
        resource: &HadalStageThumbnailV1,
        id: &ToplevelId,
    ) {
        // Dropped here rather than left for `retain_only`, because a client
        // that watches and unwatches repeatedly — which is what a dock
        // rebuilding its strip does — would otherwise accumulate dead objects
        // between window closes.
        let watchers = state.stage_state();
        if let Some(remaining) = watchers.watchers.get_mut(id) {
            remaining.retain(|watcher| watcher != resource);
            // The key goes too, not just the object. Leaving an empty vector
            // behind would grow the map by one entry per window ever watched,
            // which on a desktop left running is a slow leak with no symptom
            // until it has one.
            if remaining.is_empty() {
                watchers.watchers.remove(id);
            }
        }
    }
}

/// Wire this protocol into a compositor's dispatch tables.
///
/// One macro rather than three hand-written impls in `main.rs`, for the same
/// reason smithay ships one per protocol: the three delegations must name the
/// same user-data types, and a mismatch is a compile error a long way from the
/// mistake.
#[macro_export]
macro_rules! delegate_stage {
    ($ty:ty) => {
        smithay::reexports::wayland_server::delegate_global_dispatch!($ty: [
            $crate::stage_protocol::hadal_stage_manager_v1::HadalStageManagerV1: ()
        ] => $crate::stage_server::StageState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            $crate::stage_protocol::hadal_stage_manager_v1::HadalStageManagerV1: ()
        ] => $crate::stage_server::StageState);
        smithay::reexports::wayland_server::delegate_dispatch!($ty: [
            $crate::stage_protocol::hadal_stage_thumbnail_v1::HadalStageThumbnailV1:
                $crate::foreign_toplevel::ToplevelId
        ] => $crate::stage_server::StageState);
    };
}
