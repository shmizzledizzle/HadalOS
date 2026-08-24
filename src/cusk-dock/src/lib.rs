//! The dock's protocol clients, exposed for probing.
//!
//! The binary is the dock; this exists so `examples/probe.rs` can drive the
//! window-list client on its own. `windows.rs` talks to a compositor and cannot
//! be unit-tested end to end, so being able to run it without drawing anything
//! is the difference between "the strip is empty" and knowing why.
pub mod windows;
