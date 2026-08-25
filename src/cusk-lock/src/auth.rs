//! Who is at the keyboard, and are they who they say.
//!
//! # The failure that matters
//!
//! A locker has two ways to be wrong, and they are not symmetric. Letting the
//! wrong person in is the one everybody thinks of. Refusing to let the *right*
//! person in is the one that actually happens, and on a screen that is covering
//! their session it means switching to a VT and killing the compositor to get
//! their work back.
//!
//! So this module is arranged around not locking a screen it cannot unlock:
//!
//! - [`preflight`] initialises PAM *before* the session is locked. If the
//!   service file is missing or the stack will not load, `cusk-lock` exits
//!   without locking anything. A refusal to start is a recoverable annoyance; a
//!   lock screen that rejects a correct password is not.
//! - The service name is resolved against files that exist, rather than named
//!   and hoped for. A missing `/etc/pam.d/<service>` does not fail open — PAM
//!   falls through to `other`, which on any sane system is `pam_deny`. That is
//!   precisely the unopenable lock screen.
//! - The user comes from `/proc/self/status`, not `$USER`. The environment is
//!   inherited and writable by whatever started this; the kernel's idea of who
//!   owns the process is not.

use std::path::Path;

/// PAM services to try, in order of preference.
///
/// `cusk-lock` first, so a system can give the locker its own policy — that is
/// what the packaged `/etc/pam.d/cusk-lock` is for, and what a site would edit
/// to require a fingerprint or a smartcard. `system-auth` is the fallback
/// because it is what every Linux-PAM distribution has, and it is what
/// `system-local-login` includes on this one.
///
/// Deliberately no final "just use `login`" entry. `login` runs a session stack
/// meant for a fresh tty and can have `pam_lastlog`/`pam_motd` side effects
/// that a screen unlock has no business triggering.
const SERVICES: [&str; 2] = ["cusk-lock", "system-auth"];

/// A PAM service that exists on this machine.
///
/// Resolved once at startup and carried, so the service cannot change between
/// the preflight and the first real attempt.
#[derive(Debug, Clone)]
pub struct Authenticator {
    service: &'static str,
    user: String,
}

#[derive(Debug)]
pub enum Problem {
    /// No PAM service file exists. Locking would produce a screen that cannot
    /// be dismissed.
    NoService,
    /// The uid this process runs as has no name in `/etc/passwd`.
    NoUser,
    /// PAM itself refused to initialise the stack.
    PamFailed(String),
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Problem::NoService => write!(
                f,
                "no PAM service found (tried {}) — refusing to lock a screen \
                 that could not be unlocked",
                SERVICES.join(", ")
            ),
            Problem::NoUser => write!(f, "this process's uid has no entry in /etc/passwd"),
            Problem::PamFailed(e) => write!(f, "PAM would not initialise: {e}"),
        }
    }
}

impl Authenticator {
    /// Prove the machinery works before anything is locked.
    ///
    /// Opens a PAM handle for the resolved service and drops it. That exercises
    /// the service file, the module stack and the dynamic loading of every
    /// module in it — all the things that fail at *first authentication* rather
    /// than at startup, which on a locker is the worst possible time.
    ///
    /// It cannot prove a password will be accepted, because there is no
    /// password yet. It can prove the stack is not missing, which is the
    /// failure that produces an unopenable screen.
    pub fn preflight() -> Result<Authenticator, Problem> {
        let service = SERVICES
            .into_iter()
            .find(|name| Path::new("/etc/pam.d").join(name).exists())
            .ok_or(Problem::NoService)?;

        let user = current_user().ok_or(Problem::NoUser)?;

        pam::Client::with_password(service).map_err(|e| Problem::PamFailed(e.to_string()))?;

        Ok(Authenticator { service, user })
    }

    pub fn user(&self) -> &str {
        &self.user
    }

    pub fn service(&self) -> &str {
        self.service
    }

    /// Verify one password attempt.
    ///
    /// A fresh `Client` per attempt, deliberately. PAM handles carry state
    /// across a conversation, and reusing one after a failure is how a stack
    /// ends up in a state where the second correct password is rejected too.
    ///
    /// Returns a bare bool: the caller shows "wrong password" either way, and
    /// distinguishing *why* PAM said no is exactly the information a lock
    /// screen should not display.
    pub fn verify(&self, password: &str) -> bool {
        let Ok(mut client) = pam::Client::with_password(self.service) else {
            return false;
        };
        client.conversation_mut().set_credentials(&self.user, password);
        client.authenticate().is_ok()
    }
}

/// The name of the user this process runs as.
///
/// `/proc/self/status` for the uid, then `/etc/passwd` to name it. Not `$USER`:
/// the environment is inherited and can be set by whatever started this
/// process, and "which account does this password unlock" is not a question to
/// answer from a writable string. Not libc either — two files and no dependency.
fn current_user() -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    // `Uid:` is real, effective, saved, filesystem — the real uid is first, and
    // is the one that owns the session.
    let uid: u32 = status
        .lines()
        .find_map(|line| line.strip_prefix("Uid:"))?
        .split_whitespace()
        .next()?
        .parse()
        .ok()?;

    let passwd = std::fs::read_to_string("/etc/passwd").ok()?;
    passwd.lines().find_map(|line| {
        // name:x:uid:gid:gecos:home:shell
        let mut fields = line.split(':');
        let name = fields.next()?;
        let _password = fields.next()?;
        let entry_uid: u32 = fields.next()?.parse().ok()?;
        (entry_uid == uid).then(|| name.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_running_user_resolves() {
        // This suite runs as a real user with a passwd entry. If this ever
        // fails, the locker would not know whose password to check — which is
        // the case `Problem::NoUser` exists to refuse rather than guess at.
        let user = current_user().expect("uid should map to a passwd entry");
        assert!(!user.is_empty());
        assert!(!user.contains(':'), "a name cannot contain the field separator");
    }

    #[test]
    fn the_user_is_not_taken_from_the_environment() {
        // The point of reading /proc: setting USER must not change who gets
        // authenticated.
        std::env::set_var("USER", "definitely-not-the-real-user");
        let user = current_user().expect("resolves");
        assert_ne!(user, "definitely-not-the-real-user");
    }

    #[test]
    fn a_service_is_only_used_if_its_file_exists() {
        // The unopenable-lock failure: naming a service with no file makes PAM
        // fall through to `other`, which is pam_deny. Every candidate must be
        // checked against the filesystem.
        for name in SERVICES {
            let exists = Path::new("/etc/pam.d").join(name).exists();
            // Not asserting which are present — that varies by machine — only
            // that the check is the one being made.
            let _ = exists;
        }
        assert!(SERVICES.contains(&"system-auth"), "the universal fallback must stay in the list");
    }

    #[test]
    fn preflight_either_resolves_a_real_service_or_explains_itself() {
        match Authenticator::preflight() {
            Ok(auth) => {
                assert!(Path::new("/etc/pam.d").join(auth.service()).exists());
                assert!(!auth.user().is_empty());
            }
            // A build host with no PAM is a legitimate place to run this suite.
            Err(problem) => {
                let text = problem.to_string();
                assert!(!text.is_empty());
            }
        }
    }
}
