//! The dock's protocol clients, exposed for probing.
//!
//! The binary is the dock; this exists so `examples/probe.rs` can drive the
//! window-list client on its own. `windows.rs` talks to a compositor and cannot
//! be unit-tested end to end, so being able to run it without drawing anything
//! is the difference between "the strip is empty" and knowing why.
pub mod windows;

/// Thumbnails of minimised windows: the store the UI reads, and the two
/// conversions between a compositor's pixels and iced's.
pub mod stage;

/// Generated bindings for `hadal_stage_v1`.
///
/// Separate from `stage` so the hand-written part is not buried under macro
/// output, and public because `examples/stageprobe.rs` drives it.
pub mod stage_protocol;
