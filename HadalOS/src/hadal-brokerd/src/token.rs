//! Single-use, session-bound, expiring handles for proposed actions.
//!
//! A token is the only way to reach the executor. The model never sees one —
//! it is minted by the broker *after* a proposal has been parsed and
//! validated, and handed to the client alongside the human-readable summary.
//! So the thing the user confirms and the thing that runs are the same object
//! by construction, not by the client re-sending what it believes it saw.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::action::Action;

/// Long enough to read a rationale and think; short enough that a proposal
/// left open in a backgrounded window is not a standing grant.
const TTL: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Token(String);

impl Token {
    fn mint() -> Self {
        let mut bytes = [0u8; 32];
        // A predictable token would let one client execute another's pending
        // proposal, so this must be the CSPRNG, never a counter or a hash of
        // the content.
        getrandom::getrandom(&mut bytes).expect("system CSPRNG unavailable");
        let mut s = String::with_capacity(64);
        for b in bytes {
            s.push_str(&format!("{b:02x}"));
        }
        Token(s)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for Token {
    fn from(s: &str) -> Self {
        Token(s.to_owned())
    }
}

#[derive(Debug, Clone)]
pub struct Proposal {
    pub action: Action,
    pub rationale: String,
    pub request: u32,
    issued: Instant,
}

impl Proposal {
    fn is_expired(&self) -> bool {
        self.issued.elapsed() > TTL
    }
}

#[derive(Debug, Default)]
pub struct ProposalStore {
    pending: HashMap<Token, Proposal>,
}

impl ProposalStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, request: u32, action: Action, rationale: String) -> Token {
        self.sweep();
        let token = Token::mint();
        self.pending.insert(
            token.clone(),
            Proposal { action, rationale, request, issued: Instant::now() },
        );
        token
    }

    /// Removes and returns the proposal. Taking is the only way to read one,
    /// which is what makes a token single-use — a second `Execute` with the
    /// same token finds nothing, so a replayed confirmation cannot run an
    /// action twice.
    pub fn take(&mut self, token: &Token) -> Option<Proposal> {
        self.sweep();
        self.pending.remove(token).filter(|p| !p.is_expired())
    }

    pub fn discard(&mut self, token: &Token) -> bool {
        self.pending.remove(token).is_some()
    }

    pub fn len(&self) -> usize {
        self.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }

    fn sweep(&mut self) {
        self.pending.retain(|_, p| !p.is_expired());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::UnitName;

    fn sample() -> Action {
        Action::RestartUnit { unit: UnitName::try_from("hadald.service".to_string()).unwrap() }
    }

    #[test]
    fn tokens_are_unpredictable_and_distinct() {
        let a = Token::mint();
        let b = Token::mint();
        assert_ne!(a, b);
        assert_eq!(a.as_str().len(), 64);
        assert!(a.as_str().chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn a_token_executes_at_most_once() {
        let mut store = ProposalStore::new();
        let t = store.insert(1, sample(), "because".into());
        assert!(store.take(&t).is_some());
        assert!(store.take(&t).is_none(), "replayed token must not execute again");
        assert!(store.is_empty());
    }

    #[test]
    fn discard_prevents_execution() {
        let mut store = ProposalStore::new();
        let t = store.insert(1, sample(), "because".into());
        assert!(store.discard(&t));
        assert!(store.take(&t).is_none());
    }

    #[test]
    fn unknown_tokens_are_rejected() {
        let mut store = ProposalStore::new();
        store.insert(1, sample(), "because".into());
        assert!(store.take(&Token::from("00".repeat(32).as_str())).is_none());
    }
}
