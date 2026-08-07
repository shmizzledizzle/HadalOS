//! The closed set of actions Hadal may propose on Android, and the types that
//! make an invalid one unrepresentable.
//!
//! # The rule this file enforces
//!
//! Unchanged from the desktop broker, because it is the whole project: the
//! model never gets a shell. There is no code path from a generation to a
//! command interpreter. What the model produces is JSON; what this module
//! produces is a validated `Action`; what the executor receives is a typed
//! struct it turns into an explicit Binder call.
//!
//! `RevokePermission` holds a `PackageName` and a `RuntimePermission`, not a
//! string that gets pasted after `pm revoke`. `RestartService` holds a
//! `ServiceName`, not a property value. Those newtypes have no public
//! constructor that skips validation and deserialize through
//! `TryFrom<String>` — so **parsing is validation**. A malformed proposal does
//! not produce a suspicious `Action`; it produces no `Action` at all, and is
//! dropped before it is ever shown to the user.
//!
//! Shell metacharacters are excluded structurally rather than by escaping:
//! every validator is an allowlist over characters, so `;`, `$(`, backticks,
//! newlines and NUL are unrepresentable rather than quoted.
//!
//! # Why this matters more on Android, not less
//!
//! The desktop executor can call systemd's D-Bus API directly and never build
//! an argv. Several Android surfaces have no stable binder API reachable from
//! a native daemon and are realistically driven through `cmd`/`pm`, which
//! *does* mean an argv. The newtypes are therefore load-bearing here in a way
//! they were merely prudent on the desktop: an unvalidated package name
//! reaching `cmd package revoke` is a command injection with system uid.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::path::{Path, PathBuf};

use crate::capability::Capability;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidValue(String);

impl InvalidValue {
    /// Construct a rejection. Crate-internal: an `InvalidValue` is a statement
    /// that the broker refused something, and only the broker may make it.
    pub(crate) fn new(msg: impl Into<String>) -> Self {
        InvalidValue(msg.into())
    }
}

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
// Package names
// ─────────────────────────────────────────────────────────────────────────

/// An Android application id, e.g. `com.android.settings`.
///
/// The direct analogue of the desktop broker's `Atom`, and the same reasoning
/// applies: this is the value most likely to reach an argv, so it is the value
/// most tightly constrained. Java package syntax is already a strict
/// allowlist — letters, digits, underscore, dot-separated — which happens to
/// exclude every shell metacharacter without any special-casing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct PackageName(String);

const PACKAGE_MAX: usize = 255;

impl PackageName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PackageName {
    type Error = InvalidValue;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        if raw.is_empty() || raw.len() > PACKAGE_MAX {
            return Err(reject(format!("package name length out of range: {}", raw.len())));
        }

        let segments: Vec<&str> = raw.split('.').collect();
        // Android requires at least one dot. A bare token is either a typo or
        // an attempt to pass an option (`--user`), and guessing is exactly the
        // wrong instinct here.
        if segments.len() < 2 {
            return Err(reject("package name must contain at least one '.'"));
        }

        for seg in &segments {
            if seg.is_empty() {
                return Err(reject("package name has an empty segment"));
            }
            let mut chars = seg.chars();
            let first = chars.next().expect("segment checked non-empty");
            // A segment starting with a digit is not a valid Java identifier;
            // one starting with '-' would be read as an option.
            if !first.is_ascii_alphabetic() {
                return Err(reject(format!("segment {seg:?} does not start with a letter")));
            }
            if let Some(bad) = chars.find(|c| !(c.is_ascii_alphanumeric() || *c == '_')) {
                return Err(reject(format!("illegal character {bad:?} in package name")));
            }
        }

        Ok(PackageName(raw))
    }
}

impl From<PackageName> for String {
    fn from(p: PackageName) -> String {
        p.0
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Init service names
// ─────────────────────────────────────────────────────────────────────────

/// An init service name, as it would appear in `ctl.restart`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct ServiceName(String);

/// Restarting any of these takes the device down or takes the user's session
/// with it. Denied unconditionally, regardless of what the policy allowlist
/// says.
///
/// This mirrors the redundancy in the desktop broker's path denylist, and for
/// the same reason: it means widening the service allowlist carelessly, in a
/// config file, months from now, still cannot reboot the phone.
const CRITICAL_SERVICES: &[&str] = &[
    "init",
    "servicemanager",
    "hwservicemanager",
    "vndservicemanager",
    "zygote",
    "zygote_secondary",
    "system_server",
    "surfaceflinger",
    "vold",
    "netd",
    "ueventd",
    "healthd",
    "keystore2",
    "apexd",
];

impl ServiceName {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Whether restarting this service would take the device or session down.
    pub fn is_critical(&self) -> bool {
        CRITICAL_SERVICES.contains(&self.0.as_str())
    }
}

impl TryFrom<String> for ServiceName {
    type Error = InvalidValue;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        // init property values are length-bounded; stay well inside it.
        if raw.is_empty() || raw.len() > 64 {
            return Err(reject(format!("service name length out of range: {}", raw.len())));
        }
        if raw.starts_with('-') {
            return Err(reject("service name starts with '-'"));
        }
        if let Some(bad) = raw
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-')))
        {
            return Err(reject(format!("illegal character {bad:?} in service name")));
        }
        Ok(ServiceName(raw))
    }
}

impl From<ServiceName> for String {
    fn from(s: ServiceName) -> String {
        s.0
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Runtime permissions
// ─────────────────────────────────────────────────────────────────────────

/// A permission that can meaningfully be revoked at runtime.
///
/// A fixed set, for the same reason the desktop broker's `Keyword` is a fixed
/// set: nothing else is worth supporting from a model. Revoking an install-time
/// or signature permission is either a silent no-op or a way to break a system
/// component, and neither is a thing the user asked for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct RuntimePermission(String);

const RUNTIME_PERMISSIONS: &[&str] = &[
    "android.permission.ACCESS_BACKGROUND_LOCATION",
    "android.permission.ACCESS_COARSE_LOCATION",
    "android.permission.ACCESS_FINE_LOCATION",
    "android.permission.ACTIVITY_RECOGNITION",
    "android.permission.ADD_VOICEMAIL",
    "android.permission.ANSWER_PHONE_CALLS",
    "android.permission.BLUETOOTH_ADVERTISE",
    "android.permission.BLUETOOTH_CONNECT",
    "android.permission.BLUETOOTH_SCAN",
    "android.permission.BODY_SENSORS",
    "android.permission.CALL_PHONE",
    "android.permission.CAMERA",
    "android.permission.GET_ACCOUNTS",
    "android.permission.NEARBY_WIFI_DEVICES",
    "android.permission.POST_NOTIFICATIONS",
    "android.permission.PROCESS_OUTGOING_CALLS",
    "android.permission.READ_CALENDAR",
    "android.permission.READ_CALL_LOG",
    "android.permission.READ_CONTACTS",
    "android.permission.READ_EXTERNAL_STORAGE",
    "android.permission.READ_MEDIA_AUDIO",
    "android.permission.READ_MEDIA_IMAGES",
    "android.permission.READ_MEDIA_VIDEO",
    "android.permission.READ_PHONE_NUMBERS",
    "android.permission.READ_PHONE_STATE",
    "android.permission.READ_SMS",
    "android.permission.RECEIVE_MMS",
    "android.permission.RECEIVE_SMS",
    "android.permission.RECEIVE_WAP_PUSH",
    "android.permission.RECORD_AUDIO",
    "android.permission.SEND_SMS",
    "android.permission.USE_SIP",
    "android.permission.WRITE_CALENDAR",
    "android.permission.WRITE_CALL_LOG",
    "android.permission.WRITE_CONTACTS",
    "android.permission.WRITE_EXTERNAL_STORAGE",
];

impl RuntimePermission {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The trailing component, for display: `CAMERA` rather than the full id.
    pub fn short_name(&self) -> &str {
        self.0.rsplit('.').next().unwrap_or(&self.0)
    }
}

impl TryFrom<String> for RuntimePermission {
    type Error = InvalidValue;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        if RUNTIME_PERMISSIONS.contains(&raw.as_str()) {
            Ok(RuntimePermission(raw))
        } else {
            Err(reject(format!("not a revocable runtime permission: {raw}")))
        }
    }
}

impl From<RuntimePermission> for String {
    fn from(p: RuntimePermission) -> String {
        p.0
    }
}

// ─────────────────────────────────────────────────────────────────────────
// Log surfaces
// ─────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum LogBuffer {
    #[default]
    Main,
    System,
    Crash,
    Events,
    Kernel,
}

impl LogBuffer {
    pub fn as_logcat_arg(self) -> &'static str {
        match self {
            LogBuffer::Main => "main",
            LogBuffer::System => "system",
            LogBuffer::Crash => "crash",
            LogBuffer::Events => "events",
            LogBuffer::Kernel => "kernel",
        }
    }
}

/// A logcat tag. Constrained because it reaches a `logcat` filterspec, where
/// `:` is the level separator and would otherwise be smuggle-able.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct LogTag(String);

impl LogTag {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for LogTag {
    type Error = InvalidValue;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        if raw.is_empty() || raw.len() > 64 {
            return Err(reject("log tag length out of range"));
        }
        if raw.starts_with('-') {
            return Err(reject("log tag starts with '-'"));
        }
        if let Some(bad) = raw
            .chars()
            .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')))
        {
            return Err(reject(format!("illegal character {bad:?} in log tag")));
        }
        Ok(LogTag(raw))
    }
}

impl From<LogTag> for String {
    fn from(t: LogTag) -> String {
        t.0
    }
}

/// A DropBoxManager entry tag. A closed set — these are defined by the
/// platform, so an open string would only ever be a typo or an attack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DropBoxTag {
    DataAppCrash,
    DataAppAnr,
    DataAppNativeCrash,
    DataAppWtf,
    SystemAppCrash,
    SystemAppAnr,
    SystemAppNativeCrash,
    SystemServerCrash,
    SystemServerAnr,
    SystemServerWtf,
    SystemTombstone,
    SystemLastKmsg,
}

impl DropBoxTag {
    pub fn as_platform_tag(self) -> &'static str {
        match self {
            DropBoxTag::DataAppCrash => "data_app_crash",
            DropBoxTag::DataAppAnr => "data_app_anr",
            DropBoxTag::DataAppNativeCrash => "data_app_native_crash",
            DropBoxTag::DataAppWtf => "data_app_wtf",
            DropBoxTag::SystemAppCrash => "system_app_crash",
            DropBoxTag::SystemAppAnr => "system_app_anr",
            DropBoxTag::SystemAppNativeCrash => "system_app_native_crash",
            DropBoxTag::SystemServerCrash => "system_server_crash",
            DropBoxTag::SystemServerAnr => "system_server_anr",
            DropBoxTag::SystemServerWtf => "system_server_wtf",
            DropBoxTag::SystemTombstone => "SYSTEM_TOMBSTONE",
            DropBoxTag::SystemLastKmsg => "SYSTEM_LAST_KMSG",
        }
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
        // POSIX semantics stated outright rather than inherited from the host,
        // so that a validator built on the Windows authoring machine means the
        // same thing as one built on the device. Carried over verbatim from the
        // desktop broker; the reasoning is identical.
        if !raw.starts_with('/') {
            return Err(reject("path is not absolute"));
        }
        if raw.split('/').any(|c| c == "..") {
            return Err(reject("path contains '..'"));
        }
        Ok(SafePath(PathBuf::from(&raw)))
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
/// *after* canonicalisation and applies regardless of the allowlist.
pub struct PathPolicy {
    roots: Vec<PathBuf>,
}

impl Default for PathPolicy {
    fn default() -> Self {
        Self {
            roots: [
                "/data/anr",
                "/data/tombstones",
                "/data/system/dropbox",
                "/data/misc/logd",
                "/data/local/tmp/hadal",
                "/system/etc",
                "/vendor/etc",
            ]
            .iter()
            .map(PathBuf::from)
            .collect(),
        }
    }
}

/// Never readable, no matter what the allowlist says.
///
/// `/data/data` and `/data/user` are the whole of every app's private storage —
/// messages, tokens, databases. On a privacy-focused ROM that is the single
/// most important thing on the device, and no plausible diagnostic need
/// justifies reaching into it.
fn is_denied(path: &Path) -> bool {
    let s = path.to_string_lossy();

    const DENIED_PREFIXES: &[&str] = &[
        "/proc/",
        "/sys/",
        "/data/data/",
        "/data/user/",
        "/data/user_de/",
        "/data/misc/keystore",
        "/data/misc/vold",
        "/data/misc/adb",
        "/data/system/users/",
        "/data/system_ce/",
        "/data/system_de/",
        "/mnt/",
        "/storage/",
    ];
    const DENIED_SUFFIXES: &[&str] =
        &[".key", ".pem", ".p12", ".pfx", "id_rsa", "id_ed25519", "adb_keys"];

    if DENIED_PREFIXES.iter().any(|p| s.starts_with(p)) {
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
        // root pointing into /data/data lands here as /data/data/... and fails
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
pub enum TimeWindow {
    LastHour,
    #[default]
    LastDay,
    LastWeek,
}

impl TimeWindow {
    pub fn as_seconds(self) -> u64 {
        match self {
            TimeWindow::LastHour => 3_600,
            TimeWindow::LastDay => 86_400,
            TimeWindow::LastWeek => 604_800,
        }
    }
}

/// Per-app network policy. Models the CalyxOS Datura firewall's three states
/// rather than inventing a fourth.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum NetPolicy {
    Allow,
    /// Foreground only — background data blocked.
    BlockBackground,
    /// No network at all.
    BlockAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PrivateDnsMode {
    Off,
    Opportunistic,
    Hostname,
}

/// A DNS-over-TLS hostname.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct DnsHostname(String);

impl DnsHostname {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for DnsHostname {
    type Error = InvalidValue;

    fn try_from(raw: String) -> Result<Self, Self::Error> {
        if raw.is_empty() || raw.len() > 253 {
            return Err(reject("hostname length out of range"));
        }
        if raw.starts_with('-') || raw.starts_with('.') || raw.ends_with('.') {
            return Err(reject("malformed hostname"));
        }
        let labels: Vec<&str> = raw.split('.').collect();
        if labels.len() < 2 {
            return Err(reject("hostname must be fully qualified"));
        }
        for label in labels {
            if label.is_empty() || label.len() > 63 {
                return Err(reject("hostname label length out of range"));
            }
            if label.starts_with('-') || label.ends_with('-') {
                return Err(reject("hostname label starts or ends with '-'"));
            }
            if let Some(bad) = label.chars().find(|c| !(c.is_ascii_alphanumeric() || *c == '-')) {
                return Err(reject(format!("illegal character {bad:?} in hostname")));
            }
        }
        Ok(DnsHostname(raw))
    }
}

impl From<DnsHostname> for String {
    fn from(h: DnsHostname) -> String {
        h.0
    }
}

/// Settings changes are an enum, not a key/value pair.
///
/// A generic "write this Settings key" action would be a Settings-shaped
/// shell: the model picks the namespace, key and value, and the broker becomes
/// `settings put` with system uid. `Settings.Global` includes keys that
/// disable verification and adb authorisation, so this is not a theoretical
/// concern. Each supported change is instead a named operation with typed
/// operands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SettingChange {
    PrivateDns {
        mode: PrivateDnsMode,
        #[serde(default)]
        hostname: Option<DnsHostname>,
    },
    LocationServices {
        enabled: bool,
    },
    WifiScanAlwaysAvailable {
        enabled: bool,
    },
}

impl SettingChange {
    pub fn summary(&self) -> String {
        match self {
            SettingChange::PrivateDns { mode, hostname } => match (mode, hostname) {
                (PrivateDnsMode::Hostname, Some(h)) => {
                    format!("set Private DNS to {}", h.as_str())
                }
                (PrivateDnsMode::Hostname, None) => "set Private DNS to hostname mode".into(),
                (m, _) => format!("set Private DNS mode to {m:?}"),
            },
            SettingChange::LocationServices { enabled } => {
                format!("turn location services {}", if *enabled { "on" } else { "off" })
            }
            SettingChange::WifiScanAlwaysAvailable { enabled } => {
                format!("turn always-on Wi-Fi scanning {}", if *enabled { "on" } else { "off" })
            }
        }
    }

    /// Coherence the type system cannot express: hostname mode needs a
    /// hostname, and the other modes must not carry one.
    fn check(&self) -> Result<(), InvalidValue> {
        match self {
            SettingChange::PrivateDns { mode: PrivateDnsMode::Hostname, hostname: None } => {
                Err(reject("private DNS hostname mode requires a hostname"))
            }
            SettingChange::PrivateDns { mode, hostname: Some(_) }
                if *mode != PrivateDnsMode::Hostname =>
            {
                Err(reject("private DNS hostname given for a mode that ignores it"))
            }
            _ => Ok(()),
        }
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
    ReadLogcat {
        #[serde(default)]
        buffer: LogBuffer,
        #[serde(default)]
        tag: Option<LogTag>,
        #[serde(default = "default_lines")]
        lines: u32,
    },
    ReadCrashReport {
        tag: DropBoxTag,
        #[serde(default)]
        package: Option<PackageName>,
    },
    ReadPath {
        path: SafePath,
    },
    QueryPackage {
        package: PackageName,
    },
    ReadNetworkActivity {
        #[serde(default)]
        package: Option<PackageName>,
        #[serde(default)]
        window: TimeWindow,
    },
    ServiceStatus {
        service: ServiceName,
    },
    PermissionDiff {
        package: PackageName,
    },
    RestartService {
        service: ServiceName,
    },
    RevokePermission {
        package: PackageName,
        permission: RuntimePermission,
    },
    SetAppNetworkPolicy {
        package: PackageName,
        policy: NetPolicy,
    },
    WriteSetting {
        change: SettingChange,
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
            Action::ReadLogcat { .. } => "read-logcat",
            Action::ReadCrashReport { .. } => "read-crash-report",
            Action::ReadPath { .. } => "read-path",
            Action::QueryPackage { .. } => "query-package",
            Action::ReadNetworkActivity { .. } => "read-network-activity",
            Action::ServiceStatus { .. } => "service-status",
            Action::PermissionDiff { .. } => "permission-diff",
            Action::RestartService { .. } => "restart-service",
            Action::RevokePermission { .. } => "revoke-permission",
            Action::SetAppNetworkPolicy { .. } => "set-app-network-policy",
            Action::WriteSetting { .. } => "write-setting",
            Action::NetworkLookup { .. } => "network-lookup",
        }
    }

    pub fn capability(&self) -> Capability {
        match self {
            Action::ReadLogcat { .. } => Capability::ReadLogcat,
            Action::ReadCrashReport { .. } => Capability::ReadCrashReport,
            Action::ReadPath { .. } => Capability::ReadPath,
            Action::QueryPackage { .. } => Capability::QueryPackage,
            Action::ReadNetworkActivity { .. } => Capability::ReadNetworkActivity,
            Action::ServiceStatus { .. } => Capability::ServiceStatus,
            Action::PermissionDiff { .. } => Capability::PermissionDiff,
            Action::RestartService { .. } => Capability::RestartService,
            Action::RevokePermission { .. } => Capability::RevokePermission,
            Action::SetAppNetworkPolicy { .. } => Capability::SetAppNetworkPolicy,
            Action::WriteSetting { .. } => Capability::WriteSetting,
            Action::NetworkLookup { .. } => Capability::NetworkLookup,
        }
    }

    /// One line, shown verbatim in the confirmation prompt. The user is
    /// authorising *this*, not the model's prose about it.
    pub fn summary(&self) -> String {
        match self {
            Action::ReadLogcat { buffer, tag, lines } => match tag {
                Some(t) => format!(
                    "read {lines} lines from the {} log, tagged {}",
                    buffer.as_logcat_arg(),
                    t.as_str()
                ),
                None => format!("read {lines} lines from the {} log", buffer.as_logcat_arg()),
            },
            Action::ReadCrashReport { tag, package } => match package {
                Some(p) => format!("read {} reports for {}", tag.as_platform_tag(), p.as_str()),
                None => format!("read {} reports", tag.as_platform_tag()),
            },
            Action::ReadPath { path } => format!("read {}", path.as_path().display()),
            Action::QueryPackage { package } => format!("look up {}", package.as_str()),
            Action::ReadNetworkActivity { package, window } => match package {
                Some(p) => format!("read network usage for {} ({window:?})", p.as_str()),
                None => format!("read network usage for all apps ({window:?})"),
            },
            Action::ServiceStatus { service } => format!("check status of {}", service.as_str()),
            Action::PermissionDiff { package } => {
                format!("compare granted permissions for {}", package.as_str())
            }
            Action::RestartService { service } => format!("restart {}", service.as_str()),
            Action::RevokePermission { package, permission } => {
                format!("revoke {} from {}", permission.short_name(), package.as_str())
            }
            Action::SetAppNetworkPolicy { package, policy } => {
                format!("set network access for {} to {policy:?}", package.as_str())
            }
            Action::WriteSetting { change } => change.summary(),
            Action::NetworkLookup { query } => format!("search online for {query:?}"),
        }
    }

    /// Structural limits applied after deserialisation — the things a type
    /// cannot express on its own.
    pub fn check_limits(&self) -> Result<(), InvalidValue> {
        match self {
            Action::ReadLogcat { lines, .. } => {
                if *lines == 0 || *lines > 10_000 {
                    return Err(reject("log line count out of range"));
                }
            }
            Action::NetworkLookup { query } => {
                if query.is_empty() || query.len() > 512 {
                    return Err(reject("query length out of range"));
                }
            }
            // The denylist is enforced here rather than in `ServiceName` so
            // that `ServiceStatus` may still *report* on a critical service.
            // Reading is harmless; restarting is not.
            Action::RestartService { service } => {
                if service.is_critical() {
                    return Err(reject(format!(
                        "{} cannot be restarted without taking the device down",
                        service.as_str()
                    )));
                }
            }
            Action::WriteSetting { change } => change.check()?,
            _ => {}
        }
        Ok(())
    }
}

// ─────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn pkg(s: &str) -> Result<PackageName, InvalidValue> {
        PackageName::try_from(s.to_string())
    }
    fn svc(s: &str) -> Result<ServiceName, InvalidValue> {
        ServiceName::try_from(s.to_string())
    }
    fn path(s: &str) -> Result<SafePath, InvalidValue> {
        SafePath::try_from(s.to_string())
    }
    fn perm(s: &str) -> Result<RuntimePermission, InvalidValue> {
        RuntimePermission::try_from(s.to_string())
    }

    #[test]
    fn accepts_real_package_names() {
        for good in [
            "com.android.settings",
            "org.calyxos.datura",
            "com.google.android.gms",
            "a.b",
            "com.example.my_app2",
        ] {
            assert!(pkg(good).is_ok(), "should accept {good}");
        }
    }

    /// The whole point of the newtype. Every one of these is a command
    /// injection if it reaches an argv with system uid.
    #[test]
    fn rejects_injection_shaped_package_names() {
        for bad in [
            "com.foo; rm -rf /data",
            "com.foo && curl evil.sh",
            "$(whoami).pkg",
            "`id`.pkg",
            "com.foo\nbar",
            "com.foo bar",
            "--user",
            "-com.foo",
            "../../etc/passwd",
            "com..foo",
            "com.",
            ".com",
            "nodot",
            "",
            "com.foo\0",
            "com.2foo",
            "com/foo",
        ] {
            assert!(pkg(bad).is_err(), "should reject {bad:?}");
        }
    }

    #[test]
    fn package_length_is_bounded() {
        let long = format!("com.{}", "a".repeat(PACKAGE_MAX));
        assert!(pkg(&long).is_err());
    }

    #[test]
    fn accepts_real_service_names() {
        for good in ["hadald", "media", "wpa_supplicant", "adbd", "tombstoned"] {
            assert!(svc(good).is_ok(), "should accept {good}");
        }
    }

    #[test]
    fn rejects_bad_service_names() {
        for bad in ["zygote; reboot", "../init", "-x", "media\n", "a b", "svc$(id)", ""] {
            assert!(svc(bad).is_err(), "should reject {bad:?}");
        }
    }

    /// Restarting these takes the device down. They must be rejected at the
    /// action level even though the name itself is syntactically fine.
    #[test]
    fn critical_services_cannot_be_restarted() {
        for critical in ["zygote", "servicemanager", "system_server", "netd", "vold", "init"] {
            let a = Action::RestartService { service: svc(critical).unwrap() };
            assert!(a.check_limits().is_err(), "{critical} must not be restartable");
        }
    }

    /// ...but they may still be inspected. Reading is harmless.
    #[test]
    fn critical_services_may_still_be_inspected() {
        let a = Action::ServiceStatus { service: svc("zygote").unwrap() };
        assert!(a.check_limits().is_ok());
    }

    #[test]
    fn restartable_service_passes() {
        let a = Action::RestartService { service: svc("hadald").unwrap() };
        assert!(a.check_limits().is_ok());
    }

    #[test]
    fn permissions_are_a_fixed_set() {
        assert!(perm("android.permission.CAMERA").is_ok());
        assert!(perm("android.permission.ACCESS_FINE_LOCATION").is_ok());
        // Signature/install-time permissions are not revocable at runtime.
        assert!(perm("android.permission.INTERNET").is_err());
        assert!(perm("android.permission.INSTALL_PACKAGES").is_err());
        // Not a permission at all.
        assert!(perm("CAMERA").is_err());
        assert!(perm("android.permission.CAMERA; id").is_err());
        assert!(perm("").is_err());
    }

    #[test]
    fn permission_short_name_is_display_ready() {
        assert_eq!(perm("android.permission.CAMERA").unwrap().short_name(), "CAMERA");
    }

    #[test]
    fn rejects_traversal_and_relative_paths() {
        assert!(path("/data/anr/../../data/data/com.bank/db").is_err());
        assert!(path("relative/path").is_err());
        assert!(path("/data/anr/x\0y").is_err());
        assert!(path("..").is_err());
        assert!(path("/data/anr/traces.txt").is_ok());
    }

    /// Must hold identically on Linux and on the Windows authoring machine.
    #[test]
    fn absoluteness_is_posix_not_host_defined() {
        assert!(path("/data/anr/traces.txt").is_ok());
        assert!(path("C:\\Windows\\System32\\config\\SAM").is_err());
        assert!(path("\\\\server\\share\\file").is_err());
    }

    /// App private storage is the most sensitive thing on the device and is
    /// denied unconditionally, including via a symlink that canonicalises
    /// into it.
    #[test]
    fn denylist_covers_app_private_storage() {
        assert!(is_denied(Path::new("/data/data/com.bank/databases/accounts.db")));
        assert!(is_denied(Path::new("/data/user/0/com.signal/shared_prefs/x.xml")));
        assert!(is_denied(Path::new("/data/misc/keystore/user_0/1000_USRPKEY")));
        assert!(is_denied(Path::new("/data/misc/adb/adb_keys")));
        assert!(is_denied(Path::new("/proc/1/environ")));
        assert!(is_denied(Path::new("/storage/emulated/0/DCIM/photo.jpg")));
        assert!(is_denied(Path::new("/data/local/tmp/hadal/tls.key")));
        assert!(!is_denied(Path::new("/data/anr/traces.txt")));
        assert!(!is_denied(Path::new("/data/tombstones/tombstone_00")));
    }

    /// A malformed proposal must produce *no* action, never a partially
    /// trusted one.
    #[test]
    fn malformed_json_yields_no_action() {
        assert!(serde_json::from_str::<Action>(r#"{"action":"revoke-permission"}"#).is_err());
        assert!(serde_json::from_str::<Action>(
            r#"{"action":"revoke-permission","package":"com.foo; id","permission":"android.permission.CAMERA"}"#
        )
        .is_err());
        assert!(serde_json::from_str::<Action>(r#"{"action":"exec","cmd":"sh"}"#).is_err());
        // deny_unknown_fields: no smuggling extra operands past the executor.
        assert!(serde_json::from_str::<Action>(
            r#"{"action":"restart-service","service":"hadald","extra":"--now"}"#
        )
        .is_err());
        // An unlisted permission must fail at parse time, not at execute time.
        assert!(serde_json::from_str::<Action>(
            r#"{"action":"revoke-permission","package":"com.foo.bar","permission":"android.permission.INTERNET"}"#
        )
        .is_err());
    }

    #[test]
    fn well_formed_proposal_round_trips() {
        let a: Action = serde_json::from_str(
            r#"{"action":"revoke-permission","package":"com.example.tracker","permission":"android.permission.ACCESS_FINE_LOCATION"}"#,
        )
        .expect("should parse");
        assert_eq!(a.capability(), Capability::RevokePermission);
        assert!(a.check_limits().is_ok());
        assert!(a.summary().contains("com.example.tracker"));
        assert!(a.summary().contains("ACCESS_FINE_LOCATION"));
    }

    #[test]
    fn private_dns_requires_a_hostname_in_hostname_mode() {
        let missing = Action::WriteSetting {
            change: SettingChange::PrivateDns { mode: PrivateDnsMode::Hostname, hostname: None },
        };
        assert!(missing.check_limits().is_err());

        let stray = Action::WriteSetting {
            change: SettingChange::PrivateDns {
                mode: PrivateDnsMode::Off,
                hostname: Some(DnsHostname::try_from("dns.example.com".to_string()).unwrap()),
            },
        };
        assert!(stray.check_limits().is_err());

        let ok = Action::WriteSetting {
            change: SettingChange::PrivateDns {
                mode: PrivateDnsMode::Hostname,
                hostname: Some(DnsHostname::try_from("dns.example.com".to_string()).unwrap()),
            },
        };
        assert!(ok.check_limits().is_ok());
    }

    #[test]
    fn hostnames_reject_metacharacters_and_malformed_labels() {
        for bad in ["dns.example.com; id", "-bad.example.com", "bad-.example.com", "nodot", ".leading", "trailing.", "a..b", "$(id).com"] {
            assert!(
                DnsHostname::try_from(bad.to_string()).is_err(),
                "should reject {bad:?}"
            );
        }
        assert!(DnsHostname::try_from("dns.quad9.net".to_string()).is_ok());
    }

    #[test]
    fn log_line_count_is_bounded() {
        let a = Action::ReadLogcat { buffer: LogBuffer::Main, tag: None, lines: 0 };
        assert!(a.check_limits().is_err());
        let a = Action::ReadLogcat { buffer: LogBuffer::Main, tag: None, lines: 50_000 };
        assert!(a.check_limits().is_err());
        let a = Action::ReadLogcat { buffer: LogBuffer::Main, tag: None, lines: 200 };
        assert!(a.check_limits().is_ok());
    }

    #[test]
    fn log_tags_reject_filterspec_smuggling() {
        // ':' separates tag from level in a logcat filterspec.
        assert!(LogTag::try_from("ActivityManager:V *:S".to_string()).is_err());
        assert!(LogTag::try_from("ActivityManager".to_string()).is_ok());
    }

    #[test]
    fn every_action_maps_to_a_capability() {
        // Guards against a new variant being added without a capability.
        let samples = [
            r#"{"action":"read-logcat"}"#,
            r#"{"action":"read-crash-report","tag":"data_app_anr"}"#,
            r#"{"action":"read-path","path":"/data/anr/traces.txt"}"#,
            r#"{"action":"query-package","package":"com.android.settings"}"#,
            r#"{"action":"read-network-activity"}"#,
            r#"{"action":"service-status","service":"hadald"}"#,
            r#"{"action":"permission-diff","package":"com.android.settings"}"#,
            r#"{"action":"restart-service","service":"hadald"}"#,
            r#"{"action":"revoke-permission","package":"com.foo.bar","permission":"android.permission.CAMERA"}"#,
            r#"{"action":"set-app-network-policy","package":"com.foo.bar","policy":"block-all"}"#,
            r#"{"action":"write-setting","change":{"kind":"location-services","enabled":false}}"#,
            r#"{"action":"network-lookup","query":"android anr binder timeout"}"#,
        ];
        assert_eq!(samples.len(), Capability::ALL.len());
        for s in samples {
            let a: Action = serde_json::from_str(s).unwrap_or_else(|e| panic!("{s}: {e}"));
            let _ = a.capability();
            assert!(!a.summary().is_empty());
            assert!(a.check_limits().is_ok(), "{s} should pass limits");
        }
    }

    /// Every capability must be reachable by some action, or it is dead policy
    /// surface that Settings would render an unusable toggle for.
    #[test]
    fn every_capability_is_reachable() {
        use std::collections::HashSet;
        let samples = [
            r#"{"action":"read-logcat"}"#,
            r#"{"action":"read-crash-report","tag":"data_app_anr"}"#,
            r#"{"action":"read-path","path":"/data/anr/traces.txt"}"#,
            r#"{"action":"query-package","package":"com.android.settings"}"#,
            r#"{"action":"read-network-activity"}"#,
            r#"{"action":"service-status","service":"hadald"}"#,
            r#"{"action":"permission-diff","package":"com.android.settings"}"#,
            r#"{"action":"restart-service","service":"hadald"}"#,
            r#"{"action":"revoke-permission","package":"com.foo.bar","permission":"android.permission.CAMERA"}"#,
            r#"{"action":"set-app-network-policy","package":"com.foo.bar","policy":"block-all"}"#,
            r#"{"action":"write-setting","change":{"kind":"location-services","enabled":false}}"#,
            r#"{"action":"network-lookup","query":"x"}"#,
        ];
        let reached: HashSet<_> = samples
            .iter()
            .map(|s| serde_json::from_str::<Action>(s).unwrap().capability())
            .collect();
        for c in Capability::ALL {
            assert!(reached.contains(c), "{c} is unreachable from any action");
        }
    }
}
