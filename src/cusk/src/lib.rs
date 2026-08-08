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
pub mod theme;

/// Re-exported so the settings editor edits documents with *this* version of
/// `toml_edit`. Two crates each depending on it separately can drift onto
/// different majors, and the mismatch shows up as a type error at best and a
/// silently different parse at worst.
pub use toml_edit;
