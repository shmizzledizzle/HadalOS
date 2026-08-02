//! polkit authorization.
//!
//! The broker asks polkit about the *calling client*, not about itself. The
//! subject is the caller's unique bus name, so the authentication agent
//! prompts on the right seat, the audit record names the right user, and a
//! background process cannot borrow a foreground session's authority.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use zbus::Connection;
use zvariant::{OwnedValue, Type, Value};

use crate::capability::Capability;

/// polkit's CheckAuthorizationFlags.
const ALLOW_USER_INTERACTION: u32 = 1;

#[derive(Debug, Serialize, Deserialize, Type)]
pub struct Subject<'a> {
    kind: &'a str,
    details: HashMap<&'a str, Value<'a>>,
}

#[derive(Debug, Deserialize, Type)]
pub struct AuthorizationResult {
    pub is_authorized: bool,
    pub is_challenge: bool,
    pub details: HashMap<String, String>,
}

#[derive(Debug, Deserialize, Type)]
pub struct ActionDescription {
    pub action_id: String,
    pub description: String,
    pub message: String,
    pub vendor_name: String,
    pub vendor_url: String,
    pub icon_name: String,
    pub implicit_any: u32,
    pub implicit_inactive: u32,
    pub implicit_active: u32,
    pub annotations: HashMap<String, String>,
}

#[zbus::proxy(
    interface = "org.freedesktop.PolicyKit1.Authority",
    default_service = "org.freedesktop.PolicyKit1",
    default_path = "/org/freedesktop/PolicyKit1/Authority"
)]
trait Authority {
    fn check_authorization(
        &self,
        subject: &Subject<'_>,
        action_id: &str,
        details: HashMap<&str, &str>,
        flags: u32,
        cancellation_id: &str,
    ) -> zbus::Result<AuthorizationResult>;

    fn enumerate_actions(&self, locale: &str) -> zbus::Result<Vec<ActionDescription>>;
}

pub struct Policy {
    authority: AuthorityProxy<'static>,
}

#[derive(Debug)]
pub enum Decision {
    Allowed,
    Denied { reason: String },
}

impl Policy {
    pub async fn new(conn: &Connection) -> zbus::Result<Self> {
        Ok(Self { authority: AuthorityProxy::new(conn).await? })
    }

    /// Fail closed at startup if the shipped policy file is not installed.
    ///
    /// Without this the failure mode is silent and confusing: polkit denies
    /// unknown actions by default, so every capability would simply stop
    /// working with no indication why. Refusing to start says it once,
    /// loudly, at the point where it can still be fixed.
    pub async fn verify_actions_installed(&self) -> Result<(), String> {
        let installed = self
            .authority
            .enumerate_actions("")
            .await
            .map_err(|e| format!("cannot enumerate polkit actions: {e}"))?;

        let known: std::collections::HashSet<&str> =
            installed.iter().map(|a| a.action_id.as_str()).collect();

        let missing: Vec<String> = Capability::ALL
            .iter()
            .map(|c| c.polkit_action())
            .filter(|id| !known.contains(id.as_str()))
            .collect();

        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "polkit policy not installed — missing actions: {}. \
                 Install policy/org.hadal.broker.policy to \
                 /usr/share/polkit-1/actions/",
                missing.join(", ")
            ))
        }
    }

    /// `sender` is the caller's unique bus name (`:1.42`), taken from the
    /// message header rather than from anything the caller supplied.
    pub async fn check(
        &self,
        capability: Capability,
        sender: &str,
        summary: &str,
    ) -> Decision {
        let mut details = HashMap::new();
        details.insert("name", Value::from(sender));
        let subject = Subject { kind: "system-bus-name", details };

        // Shown in the authentication dialog, so the user is agreeing to the
        // specific operation rather than to "Hadal wants to do something".
        let mut hints: HashMap<&str, &str> = HashMap::new();
        hints.insert("polkit.message", summary);
        hints.insert("polkit.gettext_domain", "hadalos");

        let action_id = capability.polkit_action();

        match self
            .authority
            .check_authorization(&subject, &action_id, hints, ALLOW_USER_INTERACTION, "")
            .await
        {
            Ok(result) if result.is_authorized => {
                tracing::info!(capability = %capability, %sender, "authorized: {summary}");
                Decision::Allowed
            }
            Ok(result) => {
                tracing::warn!(
                    capability = %capability, %sender, challenge = result.is_challenge,
                    "denied: {summary}"
                );
                Decision::Denied {
                    reason: if result.is_challenge {
                        "authentication was not completed".into()
                    } else {
                        "not permitted by policy".into()
                    },
                }
            }
            // A broken or absent polkit means we cannot establish permission.
            // The only safe reading of "I don't know" is "no".
            Err(e) => {
                tracing::error!(capability = %capability, %sender, "polkit error: {e}");
                Decision::Denied { reason: format!("authorization service unavailable: {e}") }
            }
        }
    }
}

// Silences an unused-import warning when zvariant's OwnedValue is only needed
// by the derive machinery on some versions.
#[allow(dead_code)]
type _OwnedValue = OwnedValue;
