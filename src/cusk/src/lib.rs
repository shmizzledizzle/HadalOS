//! The parts of cusk that other programs need.
//!
//! Only the configuration schema so far, and deliberately so: this exists
//! because the settings GUI must validate against *the same* schema the
//! compositor reads, not a copy of it. A GUI with its own idea of the ranges
//! is the two-lists failure `config` was written to prevent, one process
//! further out.
//!
//! The compositor's own modules — layout, grabs, per-window state — stay in the
//! binary. They are about running a session, and nothing outside one needs them.

pub mod config;
/// Desktop entries: finding, parsing and ranking installed applications.
///
/// In the library because the launcher and the dock both need the same list.
/// A second copy would drift, and the drift would be a program that appears in
/// one and not the other.
pub mod entry;

/// The keyboard bindings, and what each one is for.
///
/// In the library because `cusk-keys` draws this list and the compositor
/// executes it. There were three hand-maintained copies before it moved here —
/// the match, the startup banner, and a test — and nothing could tell you when
/// they disagreed.
pub mod bindings;

/// What system this is, read from `os-release(5)`.
///
/// In the library because the settings editor displays it and nothing should
/// hardcode it. The identity has silently reverted once already — a baselayout
/// upgrade put its own `/etc/os-release` back on 2026-08-19 and the machine
/// called itself Gentoo for five days — so "am I HadalOS" is a question with a
/// live answer, and every component that asks must ask the same file.
pub mod identity;

/// Thumbnails of minimised windows.
///
/// In the library because the compositor produces these and the dock draws
/// them. The scaling arithmetic is the part with bugs in it and lives here so
/// it can be tested without a compositor; the capture and the protocol stay in
/// the binary.
pub mod stage;

/// The wire format for `stage`.
///
/// Generated, and separate from `stage` so that the hand-written model is not
/// buried under six hundred lines of macro output.
pub mod stage_protocol;

/// The Wayland server side of `stage`.
///
/// In the library only because `stage` is; the compositor is its only user.
/// Kept beside the model so the two cannot drift about what a thumbnail means.
pub mod stage_server;
pub mod theme;

/// What windows exist, and what may be done to them.
///
/// In the library for the same reason `entry` is: the compositor produces this
/// and the dock consumes it, and two descriptions of "what is a window" would
/// drift into a taskbar that disagrees with the compositor about which window
/// is focused. The Wayland plumbing stays in the binary; only the state model
/// and its diff live here.
pub mod toplevel;

/// The Wayland server side of `toplevel` — objects, globals and dispatch.
///
/// In the library only because `toplevel` is; the compositor is its only user.
/// Kept beside the model so the two cannot drift about what an event means.
pub mod foreign_toplevel;

/// Re-exported so the settings editor edits documents with *this* version of
/// `toml_edit`. Two crates each depending on it separately can drift onto
/// different majors, and the mismatch shows up as a type error at best and a
/// silently different parse at worst.
pub use toml_edit;
