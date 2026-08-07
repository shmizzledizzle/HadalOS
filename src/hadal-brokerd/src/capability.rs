//! The closed set of things Hadal is able to ask for, on Android.
//!
//! # Relationship to the desktop broker
//!
//! The tier model is carried over unchanged from HadalOS/src/hadal-brokerd —
//! Read, Inspect, Mutate, Egress, with Mutate always requiring a fresh
//! authorization and Egress denied by default. That structure is the part of
//! the design that is actually portable.
//!
//! What is *not* portable is the capability set itself. The desktop broker's
//! capabilities are Portage- and systemd-shaped (`emerge-apply`,
//! `restart-unit`, `read-portage-log`) and none of those concepts exist here.
//! Substituting them one-for-one would produce a broker that speaks about
//! things Android does not have.
//!
//! # Where the authorization decision comes from
//!
//! The desktop maps each capability 1:1 to a polkit action id. Android has no
//! polkit, so the mapping is 1:1 to a *permission string plus a confirmation
//! contract*:
//!
//! - Read/Inspect tiers are gated by a signature-level Android permission held
//!   only by the shell surface, checked with `checkCallingPermission` in the
//!   Binder transaction.
//! - Mutate tier additionally requires a confirmation Activity owned by the
//!   system, launched with `HIDE_NON_SYSTEM_OVERLAY_WINDOWS`, so the calling
//!   app cannot draw over the prompt. That overlay-suppression is the direct
//!   analogue of polkit's `auth_admin` being unspoofable by the client — on
//!   Android the threat is tapjacking rather than a fake dialog, but the
//!   requirement is identical: **the user must be authorising the real action.**
//! - Egress is additionally gated by uid group membership, see §2 of
//!   ARCHITECTURE.md.
//!
//! Adding a variant here is a deliberate act: it requires a matching entry in
//! `policy/hadal_capabilities.xml` and a matching SELinux rule, and
//! `Capability::ALL` is checked against the installed policy at startup.

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Exposes system state the active user could already read in Settings or
    /// a log viewer.
    Read,
    /// Resolves and reports; no side effects.
    Inspect,
    /// Changes the device. Confirmed by the user, every single time.
    Mutate,
    /// Leaves the device. Off unless explicitly turned on.
    Egress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    // ── Read ────────────────────────────────────────────────────────────
    /// logcat, by buffer and tag.
    ReadLogcat,
    /// DropBoxManager entries — crashes, ANRs, tombstones, kernel panics.
    /// The flagship diagnostic surface; see ARCHITECTURE.md §2.5.
    ReadCrashReport,
    /// A file, under a prefix allowlist.
    ReadPath,
    /// PackageManager metadata for an installed package.
    QueryPackage,
    /// Per-app network counters. The second flagship surface — this is what
    /// makes "which apps phoned home overnight" answerable.
    ReadNetworkActivity,

    // ── Inspect ─────────────────────────────────────────────────────────
    /// State of an init or system service.
    ServiceStatus,
    /// What a package currently holds vs. what it declares. Read-only; the
    /// analogue of `emerge --pretend` in that it resolves and reports without
    /// touching anything.
    PermissionDiff,

    // ── Mutate ──────────────────────────────────────────────────────────
    /// `ctl.restart` on an allowlisted init service.
    RestartService,
    /// Drop a runtime permission from an app.
    RevokePermission,
    /// Per-app network policy — the CalyxOS Datura firewall toggle.
    SetAppNetworkPolicy,
    /// A named, enumerated Settings change. Never an arbitrary key/value.
    WriteSetting,

    // ── Egress ──────────────────────────────────────────────────────────
    NetworkLookup,
}

impl Capability {
    pub const ALL: &'static [Capability] = &[
        Capability::ReadLogcat,
        Capability::ReadCrashReport,
        Capability::ReadPath,
        Capability::QueryPackage,
        Capability::ReadNetworkActivity,
        Capability::ServiceStatus,
        Capability::PermissionDiff,
        Capability::RestartService,
        Capability::RevokePermission,
        Capability::SetAppNetworkPolicy,
        Capability::WriteSetting,
        Capability::NetworkLookup,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Capability::ReadLogcat => "read-logcat",
            Capability::ReadCrashReport => "read-crash-report",
            Capability::ReadPath => "read-path",
            Capability::QueryPackage => "query-package",
            Capability::ReadNetworkActivity => "read-network-activity",
            Capability::ServiceStatus => "service-status",
            Capability::PermissionDiff => "permission-diff",
            Capability::RestartService => "restart-service",
            Capability::RevokePermission => "revoke-permission",
            Capability::SetAppNetworkPolicy => "set-app-network-policy",
            Capability::WriteSetting => "write-setting",
            Capability::NetworkLookup => "network-lookup",
        }
    }

    /// The Android permission string guarding the Binder transaction.
    ///
    /// Deliberately *not* one permission per capability at the manifest level —
    /// Android permissions are granted per-app, and every caller we ship is the
    /// same app. The per-capability granularity lives in the policy table and
    /// the confirmation contract; this string is the coarse "may you talk to
    /// the broker at all" gate.
    pub fn android_permission(self) -> &'static str {
        match self.tier() {
            Tier::Read | Tier::Inspect => "android.hadal.permission.QUERY",
            Tier::Mutate => "android.hadal.permission.PROPOSE_MUTATION",
            Tier::Egress => "android.hadal.permission.EGRESS",
        }
    }

    /// Stable id used in the policy XML and in Settings' per-capability
    /// allow/ask/never store.
    pub fn policy_key(self) -> String {
        format!("hadal.capability.{}", self.id())
    }

    pub fn tier(self) -> Tier {
        match self {
            Capability::ReadLogcat
            | Capability::ReadCrashReport
            | Capability::ReadPath
            | Capability::QueryPackage
            | Capability::ReadNetworkActivity => Tier::Read,

            Capability::ServiceStatus | Capability::PermissionDiff => Tier::Inspect,

            Capability::RestartService
            | Capability::RevokePermission
            | Capability::SetAppNetworkPolicy
            | Capability::WriteSetting => Tier::Mutate,

            Capability::NetworkLookup => Tier::Egress,
        }
    }

    /// Whether executing this capability requires the system-owned
    /// confirmation Activity to return an affirmative result.
    ///
    /// This is the Android replacement for polkit `auth_admin`. It is derived
    /// from the tier rather than stored per-capability so that a new Mutate
    /// variant cannot be added without inheriting the prompt.
    pub fn requires_confirmation(self) -> bool {
        matches!(self.tier(), Tier::Mutate | Tier::Egress)
    }

    /// What `AvailableCapabilities` reports before any confirmation
    /// round-trip, so the shell can grey out affordances. Advisory only — the
    /// real decision is always made at `Execute` time.
    pub fn advisory_disposition(self) -> &'static str {
        match self.tier() {
            Tier::Read | Tier::Inspect => "allow",
            Tier::Mutate => "auth",
            Tier::Egress => "deny",
        }
    }

    /// Human-readable, shown in confirmation UI next to the model's rationale.
    pub fn describe(self) -> &'static str {
        match self {
            Capability::ReadLogcat => "read the system log",
            Capability::ReadCrashReport => "read a crash or ANR report",
            Capability::ReadPath => "read a file",
            Capability::QueryPackage => "look up app information",
            Capability::ReadNetworkActivity => "read per-app network usage",
            Capability::ServiceStatus => "check a system service's status",
            Capability::PermissionDiff => "compare an app's granted permissions",
            Capability::RestartService => "restart a system service",
            Capability::RevokePermission => "revoke an app's permission",
            Capability::SetAppNetworkPolicy => "change an app's network access",
            Capability::WriteSetting => "change a device setting",
            Capability::NetworkLookup => "search online documentation",
        }
    }
}

impl fmt::Display for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn ids_are_unique() {
        let ids: HashSet<_> = Capability::ALL.iter().map(|c| c.id()).collect();
        assert_eq!(ids.len(), Capability::ALL.len());
    }

    #[test]
    fn every_mutating_capability_requires_authorization() {
        for c in Capability::ALL {
            if c.tier() == Tier::Mutate {
                assert_eq!(c.advisory_disposition(), "auth", "{c} must not be advisory-allow");
                assert!(c.requires_confirmation(), "{c} must prompt");
            }
        }
    }

    #[test]
    fn egress_is_denied_by_default() {
        assert_eq!(Capability::NetworkLookup.advisory_disposition(), "deny");
        assert!(Capability::NetworkLookup.requires_confirmation());
    }

    /// Read and Inspect must never silently acquire a prompt requirement —
    /// if they do, the shell's greyed-out affordances become wrong.
    #[test]
    fn read_and_inspect_do_not_prompt() {
        for c in Capability::ALL {
            if matches!(c.tier(), Tier::Read | Tier::Inspect) {
                assert!(!c.requires_confirmation(), "{c} should not prompt");
            }
        }
    }

    /// The coarse permission gate must still separate mutation from query;
    /// a bug here would let a query-only caller propose a mutation.
    #[test]
    fn permission_strings_separate_tiers() {
        assert_ne!(
            Capability::ReadLogcat.android_permission(),
            Capability::RestartService.android_permission()
        );
        assert_ne!(
            Capability::RestartService.android_permission(),
            Capability::NetworkLookup.android_permission()
        );
    }

    #[test]
    fn policy_keys_are_unique_and_namespaced() {
        let keys: HashSet<_> = Capability::ALL.iter().map(|c| c.policy_key()).collect();
        assert_eq!(keys.len(), Capability::ALL.len());
        assert!(Capability::ALL.iter().all(|c| c.policy_key().starts_with("hadal.capability.")));
    }
}
