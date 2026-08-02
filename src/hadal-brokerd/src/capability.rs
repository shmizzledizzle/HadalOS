//! The closed set of things Hadal is able to ask for.
//!
//! Every capability maps 1:1 to a polkit action id, which is what lets the
//! Settings surface offer per-capability allow/ask/never without inventing a
//! parallel policy store, and lets a site admin lock any single one down with
//! a stock polkit rule instead of patching HadalOS.
//!
//! Adding a variant here is a deliberate act: it requires a matching entry in
//! `policy/org.hadal.broker.policy`, and `Capability::ALL` is checked against
//! the installed policy file at startup (see `policy::verify_actions_installed`).

use std::fmt;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tier {
    /// Exposes system state a local active session could already read.
    Read,
    /// Resolves and reports; no side effects.
    Inspect,
    /// Changes the system. `auth_admin`, every single time.
    Mutate,
    /// Leaves the machine. Off unless explicitly turned on.
    Egress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Capability {
    ReadJournal,
    ReadPortageLog,
    ReadPath,
    QueryPackage,
    UnitStatus,
    EmergePretend,
    RestartUnit,
    EmergeApply,
    WriteConfig,
    NetworkLookup,
}

impl Capability {
    pub const ALL: &'static [Capability] = &[
        Capability::ReadJournal,
        Capability::ReadPortageLog,
        Capability::ReadPath,
        Capability::QueryPackage,
        Capability::UnitStatus,
        Capability::EmergePretend,
        Capability::RestartUnit,
        Capability::EmergeApply,
        Capability::WriteConfig,
        Capability::NetworkLookup,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Capability::ReadJournal => "read-journal",
            Capability::ReadPortageLog => "read-portage-log",
            Capability::ReadPath => "read-path",
            Capability::QueryPackage => "query-package",
            Capability::UnitStatus => "unit-status",
            Capability::EmergePretend => "emerge-pretend",
            Capability::RestartUnit => "restart-unit",
            Capability::EmergeApply => "emerge-apply",
            Capability::WriteConfig => "write-config",
            Capability::NetworkLookup => "network-lookup",
        }
    }

    pub fn polkit_action(self) -> String {
        format!("org.hadal.broker.{}", self.id())
    }

    pub fn tier(self) -> Tier {
        match self {
            Capability::ReadJournal
            | Capability::ReadPortageLog
            | Capability::ReadPath
            | Capability::QueryPackage => Tier::Read,

            Capability::UnitStatus | Capability::EmergePretend => Tier::Inspect,

            Capability::RestartUnit | Capability::EmergeApply | Capability::WriteConfig => {
                Tier::Mutate
            }

            Capability::NetworkLookup => Tier::Egress,
        }
    }

    /// What `AvailableCapabilities` reports before any polkit round-trip, so
    /// clients can grey out affordances. Advisory only — the real decision is
    /// always made by polkit at `Execute` time.
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
            Capability::ReadJournal => "read the system journal",
            Capability::ReadPortageLog => "read a Portage build log",
            Capability::ReadPath => "read a file",
            Capability::QueryPackage => "look up package information",
            Capability::UnitStatus => "check a service's status",
            Capability::EmergePretend => "simulate a package operation",
            Capability::RestartUnit => "restart a system service",
            Capability::EmergeApply => "install or remove packages",
            Capability::WriteConfig => "change a configuration setting",
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
            }
        }
    }

    #[test]
    fn egress_is_denied_by_default() {
        assert_eq!(Capability::NetworkLookup.advisory_disposition(), "deny");
    }
}
