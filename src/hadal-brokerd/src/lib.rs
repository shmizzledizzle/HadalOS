//! `hadal-brokerd` for Android — capability broker for the Hadal system
//! component.
//!
//! This crate is the Android counterpart to HadalOS's `src/hadal-brokerd`. It
//! shares that crate's architecture and none of its vocabulary; see
//! ARCHITECTURE.md §1 for what carries over and what does not.
//!
//! The layering is deliberately identical to the desktop broker so that the
//! two can be reasoned about together:
//!
//! ```text
//!   model output (JSON)
//!        │
//!        ▼
//!   action::Action        parsing is validation — an invalid proposal
//!        │                produces no Action at all
//!        ▼
//!   capability::Capability  what is being permitted, and at which tier
//!        │
//!        ▼
//!   confirmation           system-owned Activity for Mutate/Egress
//!        │
//!        ▼
//!   executor               typed struct → explicit Binder call
//! ```

pub mod action;
pub mod capability;
pub mod plan;

pub use action::{Action, InvalidValue};
pub use capability::{Capability, Tier};

/// Parse a model proposal into a validated action.
///
/// The single entry point from untrusted input. Deserialisation enforces every
/// newtype's validator; `check_limits` then applies the structural bounds a
/// type cannot express on its own. Callers must not construct `Action` values
/// from model output by any other route.
pub fn parse_proposal(json: &str) -> Result<Action, InvalidValue> {
    let action: Action =
        serde_json::from_str(json).map_err(|e| InvalidValue::from_parse_error(&e.to_string()))?;
    action.check_limits()?;
    Ok(action)
}

impl InvalidValue {
    fn from_parse_error(msg: &str) -> Self {
        // Deliberately does not echo the offending input back. A rejected
        // proposal may contain attacker-influenced text, and the rejection
        // reason is surfaced in logs and potentially in UI.
        InvalidValue::new(format!("proposal did not parse: {msg}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_a_well_formed_proposal() {
        let a = parse_proposal(
            r#"{"action":"read-crash-report","tag":"data_app_anr","package":"com.example.app"}"#,
        )
        .expect("should parse");
        assert_eq!(a.capability(), Capability::ReadCrashReport);
        assert_eq!(a.capability().tier(), Tier::Read);
        assert!(!a.capability().requires_confirmation());
    }

    #[test]
    fn a_mutation_is_parsed_but_flagged_for_confirmation() {
        let a = parse_proposal(
            r#"{"action":"set-app-network-policy","package":"com.example.tracker","policy":"block-all"}"#,
        )
        .expect("should parse");
        assert_eq!(a.capability().tier(), Tier::Mutate);
        assert!(a.capability().requires_confirmation());
    }

    #[test]
    fn limits_are_applied_at_the_entry_point() {
        // Parses cleanly as JSON; rejected by check_limits.
        assert!(parse_proposal(r#"{"action":"restart-service","service":"zygote"}"#).is_err());
        assert!(parse_proposal(r#"{"action":"read-logcat","lines":999999}"#).is_err());
    }

    #[test]
    fn there_is_no_exec_action() {
        for attempt in [
            r#"{"action":"exec","cmd":"sh -c id"}"#,
            r#"{"action":"shell","command":"whoami"}"#,
            r#"{"action":"run","argv":["sh"]}"#,
        ] {
            assert!(parse_proposal(attempt).is_err(), "{attempt} must not parse");
        }
    }

    /// The rejection reason must not echo attacker-influenced input back into
    /// logs or UI.
    #[test]
    fn rejection_does_not_echo_input() {
        let err = parse_proposal(r#"{"action":"query-package","package":"com.evil$(id)"}"#)
            .expect_err("should reject");
        assert!(!err.to_string().contains("$(id)"), "error echoed input: {err}");
    }
}
