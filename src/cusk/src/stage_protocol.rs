//! Generated bindings for `hadal_stage_v1`, server side.
//!
//! The XML lives at `protocol/hadal-stage-v1.xml` in the repository root, one
//! copy, generated from here for the compositor and from `cusk-dock` for the
//! client. A protocol description is exactly the kind of thing that must not
//! exist twice: two copies drifting is a compositor and a dock that disagree
//! about a wire format, which shows up as a parse error with no obvious cause.
//!
//! The shape of this module — a `generated` wrapper holding `__interfaces` and
//! then the code — is not a choice. It is what `wayland-scanner` emits into,
//! and it is copied from `wayland-protocols-wlr`'s own `wayland_protocol!`
//! macro so that an out-of-tree protocol is built the same way an in-tree one
//! is.
//!
//! The `use` lines are load-bearing. `watch` takes a
//! `zwlr_foreign_toplevel_handle_v1` argument, and the scanner emits an
//! unqualified path for it; without that import in scope the generated code
//! does not compile, and the error names a type rather than a missing import.

#![allow(
    dead_code,
    non_camel_case_types,
    unused_unsafe,
    unused_variables,
    non_upper_case_globals,
    non_snake_case,
    unused_imports,
    missing_docs,
    clippy::all
)]

use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::*;
use smithay::reexports::wayland_server;
use smithay::reexports::wayland_server::protocol::*;

pub mod __interfaces {
    use smithay::reexports::wayland_protocols_wlr::foreign_toplevel::v1::server::__interfaces::*;
    use smithay::reexports::wayland_server::protocol::__interfaces::*;
    wayland_scanner::generate_interfaces!("../../protocol/hadal-stage-v1.xml");
}
use self::__interfaces::*;

wayland_scanner::generate_server_code!("../../protocol/hadal-stage-v1.xml");
