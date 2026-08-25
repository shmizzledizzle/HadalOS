//! What system this is, according to the system.
//!
//! Read from `os-release(5)` rather than compiled in. A HadalOS component that
//! hardcodes the string "HadalOS" is a component that will keep saying HadalOS
//! on a machine that has stopped being one — and that is not hypothetical here.
//! On 2026-08-19 a `sys-apps/baselayout` upgrade restored its own
//! `/etc/os-release` symlink over the file `sys-apps/hadalos-release` installs,
//! and the machine identified as Gentoo for five days without anything noticing.
//! An About panel that had been hardcoded would have been the most confident
//! liar on the system.
//!
//! So this reads the file, and `Identity::is_hadalos` is a *question*, not an
//! assumption. Callers that want to display the identity can also display when
//! it is wrong.
//!
//! # Precedence
//!
//! os-release(5) is explicit: `/etc/os-release` wins, `/usr/lib/os-release` is
//! the vendor fallback, and a derived distribution overrides the former without
//! owning the latter. That split is the entire mechanism `hadalos-release`
//! relies on, so it is honoured here rather than reading one path and hoping.

use std::path::{Path, PathBuf};

/// The two paths os-release(5) defines, in the order it defines them.
const PATHS: [&str; 2] = ["/etc/os-release", "/usr/lib/os-release"];

/// What HadalOS's own `os-release` sets `ID` to.
///
/// The one string that is compiled in, because it is the thing being *tested
/// for* rather than displayed. Everything shown to the user comes from the file.
pub const HADALOS_ID: &str = "hadalos";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// `PRETTY_NAME`, or `NAME`, or the id — in that order, because os-release
    /// only guarantees `NAME` and `ID`, and a panel with an empty title is
    /// worse than one showing a lowercase id.
    pub pretty_name: String,
    pub id: String,
    /// `ID_LIKE`. Present on a derived distribution and absent on the thing it
    /// derives from, so its absence is meaningful rather than missing data.
    pub id_like: Option<String>,
    pub version: Option<String>,
    pub home_url: Option<String>,
    pub bug_url: Option<String>,
    /// Which of the two paths this actually came from.
    ///
    /// Worth surfacing: reading from `/usr/lib` means `/etc/os-release` is
    /// absent, which is the exact state the hadalos-release ebuild documents as
    /// its re-merge hazard — CONTENTS claiming a file that is not on disk.
    pub source: PathBuf,
}

impl Identity {
    /// Read the system's identity, honouring os-release(5) precedence.
    ///
    /// `None` when neither file exists or neither parses into anything with an
    /// `ID`. That is a real state — a container with no os-release at all — and
    /// is left for the caller to phrase, because "unknown system" reads very
    /// differently in an About panel than in a log line.
    pub fn load() -> Option<Identity> {
        PATHS.iter().find_map(|path| Identity::read(Path::new(path)))
    }

    fn read(path: &Path) -> Option<Identity> {
        let text = std::fs::read_to_string(path).ok()?;
        let mut identity = Identity::parse(&text)?;
        identity.source = path.to_path_buf();
        Some(identity)
    }

    /// Parse os-release content. Separate from the file read so it is testable
    /// without writing to /etc.
    pub fn parse(text: &str) -> Option<Identity> {
        let mut fields: Vec<(String, String)> = Vec::new();
        for line in text.lines() {
            let line = line.trim();
            // Comments and blanks are legal in os-release and appear in
            // HadalOS's own file, which explains its ID_LIKE in a comment.
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((key, value)) = line.split_once('=') else { continue };
            fields.push((key.trim().to_string(), unquote(value.trim())));
        }

        let get = |name: &str| {
            fields
                .iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
                .filter(|value| !value.is_empty())
        };

        // ID is the field everything else is keyed off, and the one os-release
        // guarantees. Without it there is nothing to report.
        let id = get("ID")?;
        Some(Identity {
            pretty_name: get("PRETTY_NAME").or_else(|| get("NAME")).unwrap_or_else(|| id.clone()),
            id_like: get("ID_LIKE"),
            version: get("VERSION").or_else(|| get("VERSION_ID")),
            home_url: get("HOME_URL"),
            bug_url: get("BUG_REPORT_URL"),
            id,
            source: PathBuf::new(),
        })
    }

    /// Whether this machine currently identifies as HadalOS.
    ///
    /// A question rather than an assertion. See the module docs for the five
    /// days this was silently false.
    pub fn is_hadalos(&self) -> bool {
        self.id == HADALOS_ID
    }
}

/// Strip one layer of shell quoting.
///
/// os-release values are shell-compatible, and both HadalOS's file and
/// Gentoo's use a mix — `ID=hadalos` unquoted beside `PRETTY_NAME="HadalOS"`.
/// Only the outer pair is removed and escapes are left alone: the values in
/// practice are names and URLs, and a half-implemented unescaper that mangles a
/// URL is worse than none.
fn unquote(value: &str) -> String {
    let bytes = value.as_bytes();
    if bytes.len() >= 2 {
        let first = bytes[0];
        let last = bytes[bytes.len() - 1];
        if (first == b'"' || first == b'\'') && first == last {
            return value[1..value.len() - 1].to_string();
        }
    }
    value.to_string()
}

/// The running kernel, as `uname -r` reports it.
///
/// From procfs rather than the `uname` syscall, so this needs no libc. The file
/// is one line and has been in the same place since Linux 1.0.
pub fn kernel_release() -> Option<String> {
    std::fs::read_to_string("/proc/sys/kernel/osrelease")
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The architecture this binary was built for.
///
/// Compile-time, and therefore the honest answer to "what is this program",
/// which is what an About panel is being asked. `uname -m` would answer "what
/// can this machine run", which is a different question and differs on exactly
/// the machines where it matters — a 32-bit build on a 64-bit kernel.
pub fn architecture() -> &'static str {
    std::env::consts::ARCH
}

#[cfg(test)]
mod tests {
    use super::*;

    const HADALOS: &str = r#"
NAME="HadalOS"
ID=hadalos
# Everything underneath is still Gentoo.
ID_LIKE=gentoo
PRETTY_NAME="HadalOS"
VERSION="0.1 (foundation)"
VERSION_ID="0.1"
ANSI_COLOR="1;36"
HOME_URL="https://github.com/shmizzledizzle/HadalOS"
BUG_REPORT_URL="https://github.com/shmizzledizzle/HadalOS/issues"
"#;

    const GENTOO: &str = r#"
NAME='Gentoo'
ID='gentoo'
PRETTY_NAME='Gentoo Linux'
VERSION='2.18'
ANSI_COLOR='1;32'
"#;

    #[test]
    fn hadalos_identifies_itself() {
        let id = Identity::parse(HADALOS).expect("parses");
        assert_eq!(id.pretty_name, "HadalOS");
        assert_eq!(id.id, "hadalos");
        assert_eq!(id.id_like.as_deref(), Some("gentoo"));
        assert_eq!(id.version.as_deref(), Some("0.1 (foundation)"));
        assert!(id.is_hadalos());
    }

    #[test]
    fn the_reverted_machine_is_reported_as_what_it_is() {
        // The five-day state: baselayout's file, on a machine that still has
        // every HadalOS package installed. Nothing here may claim otherwise.
        let id = Identity::parse(GENTOO).expect("parses");
        assert_eq!(id.pretty_name, "Gentoo Linux");
        assert!(!id.is_hadalos(), "a Gentoo os-release must not read as HadalOS");
        assert_eq!(id.id_like, None, "Gentoo derives from nothing");
    }

    #[test]
    fn single_and_double_quotes_are_both_stripped() {
        // Gentoo's file uses single quotes and HadalOS's uses double. A reader
        // that handled only one would show HadalOS correctly and Gentoo as
        // `'Gentoo Linux'`, which is the wrong way round for a mismatch warning.
        assert_eq!(unquote("\"HadalOS\""), "HadalOS");
        assert_eq!(unquote("'Gentoo Linux'"), "Gentoo Linux");
        assert_eq!(unquote("hadalos"), "hadalos");
        // Mismatched quotes are left alone rather than half-stripped.
        assert_eq!(unquote("\"unterminated"), "\"unterminated");
    }

    #[test]
    fn comments_and_blank_lines_do_not_become_fields() {
        // HadalOS's own os-release carries a four-line comment explaining
        // ID_LIKE, so this is the file actually shipped, not a hypothetical.
        let id = Identity::parse("# a comment\n\nID=x\n# ID=wrong\n").expect("parses");
        assert_eq!(id.id, "x");
    }

    #[test]
    fn a_file_without_an_id_is_not_an_identity() {
        assert!(Identity::parse("PRETTY_NAME=\"Something\"\n").is_none());
        assert!(Identity::parse("").is_none());
        // Present but empty is absent: an empty ID names nothing.
        assert!(Identity::parse("ID=\n").is_none());
    }

    #[test]
    fn pretty_name_falls_back_rather_than_being_blank() {
        // os-release only guarantees NAME and ID.
        assert_eq!(Identity::parse("ID=x\nNAME=Ex\n").unwrap().pretty_name, "Ex");
        assert_eq!(Identity::parse("ID=x\n").unwrap().pretty_name, "x");
    }

    #[test]
    fn version_falls_back_to_version_id() {
        let id = Identity::parse("ID=x\nVERSION_ID=\"9\"\n").unwrap();
        assert_eq!(id.version.as_deref(), Some("9"));
    }

    #[test]
    fn the_real_system_parses_if_it_has_an_os_release() {
        // Not asserting *which* system: this suite runs on build hosts too.
        // Asserting only that whatever is there does not crash the reader and
        // produces a non-empty name, which is what the panel needs.
        if let Some(id) = Identity::load() {
            assert!(!id.pretty_name.is_empty());
            assert!(!id.id.is_empty());
            assert!(PATHS.iter().any(|p| id.source == Path::new(p)));
        }
    }
}
