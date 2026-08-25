//! Generated bindings for `hadal_stage_v1`, client side.
//!
//! The same XML the compositor generates from — `protocol/hadal-stage-v1.xml`
//! in the repository root, reached by the same relative path because both
//! crates sit one level under `src/`. One description, two generations. A
//! second copy of the XML would be two descriptions of a wire format, and the
//! first time they drifted the symptom would be a protocol error with nothing
//! in either file to explain it.
//!
//! See `cusk::stage_protocol` for why the module is shaped this way; it is
//! `wayland-protocols-wlr`'s own arrangement, and the `use` lines are what let
//! the scanner resolve the `zwlr_foreign_toplevel_handle_v1` argument to
//! `watch`.

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

use wayland_client;
use wayland_client::protocol::*;
use wayland_protocols_wlr::foreign_toplevel::v1::client::*;

pub mod __interfaces {
    use wayland_client::protocol::__interfaces::*;
    use wayland_protocols_wlr::foreign_toplevel::v1::client::__interfaces::*;
    wayland_scanner::generate_interfaces!("../../protocol/hadal-stage-v1.xml");
}
use self::__interfaces::*;

wayland_scanner::generate_client_code!("../../protocol/hadal-stage-v1.xml");
