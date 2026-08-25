//! The dock's protocol clients, exposed for probing.
//!
//! The binary is the dock; this exists so `examples/probe.rs` can drive the
//! window-list client on its own. `windows.rs` talks to a compositor and cannot
//! be unit-tested end to end, so being able to run it without drawing anything
//! is the difference between "the strip is empty" and knowing why.
pub mod windows;

/// How much charge is left. Pure sysfs, no daemon.
pub mod battery;

/// Whether there is a network and what kind. NetworkManager, falling back to
/// sysfs.
pub mod network;

/// The two readouts together, polled off the UI thread.
pub mod status;

/// Thumbnails of minimised windows: the store the UI reads, and the two
/// conversions between a compositor's pixels and iced's.
pub mod stage;

/// Generated bindings for `hadal_stage_v1`.
///
/// Separate from `stage` so the hand-written part is not buried under macro
/// output, and public because `examples/stageprobe.rs` drives it.
pub mod stage_protocol;
