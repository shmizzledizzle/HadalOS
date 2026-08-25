//! Session actions — log out, switch user, suspend, restart, shut down.
//!
//! All of it goes through systemd-logind, and one item through the display
//! manager. The dock asks; it never does any of this itself. That matters more
//! than it sounds: a dock that ran `systemctl poweroff` would be a desktop
//! applet with a privilege story, and there is already a component in this tree
//! whose whole job is holding privilege (hadal-brokerd). logind is the one that
//! already knows whether this session is allowed to power the machine off, and
//! it answers that question per session rather than per user.
//!
//! # Why the availability query is separate from the action
//!
//! `CanSuspend` and friends return one of `yes`, `no`, `na`, or `challenge` —
//! four answers, not a boolean. `na` means the system cannot do it at all (no
//! swap, no suspend support); `no` means policy forbids it; `challenge` means
//! it would work but needs authentication first.
//!
//! Asked once at start-up and cached, because these do not change over a
//! session and a menu that queries four D-Bus methods every time it opens is a
//! menu that stutters. The cost of being wrong is small and self-correcting:
//! the action still goes to logind, which is the authority, and a stale `yes`
//! becomes a failed call rather than a wrong outcome.

use std::time::Duration;

/// What the user picked.
///
/// Deliberately closed. Every variant here maps to exactly one D-Bus call with
/// no free parameters, so there is no path from a click to an arbitrary
/// command — the same argument the broker's capability table makes, at a much
/// smaller scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    SwitchUser,
    LogOut,
    Lock,
    Suspend,
    Restart,
    ShutDown,
}

impl Action {
    /// Every action, in the order the menu shows them.
    ///
    /// The menu derives its two groups by partitioning this on `is_final`
    /// rather than listing the actions twice. Two lists would be two places to
    /// add the next action, and the failure mode of forgetting one is an item
    /// that exists, is reachable by nothing, and looks like a rendering bug.
    pub const ALL: [Action; 6] = [
        Action::Lock,
        Action::SwitchUser,
        Action::Suspend,
        Action::LogOut,
        Action::Restart,
        Action::ShutDown,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Action::SwitchUser => "Switch User",
            Action::LogOut => "Log Out",
            Action::Lock => "Lock",
            Action::Suspend => "Suspend",
            Action::Restart => "Restart",
            Action::ShutDown => "Shut Down",
        }
    }

    /// Whether picking this ends the session or the uptime.
    ///
    /// Used to separate the menu, not to confirm: a dock is not the right place
    /// to argue with someone who has just clicked Shut Down, and logind will
    /// refuse if another session objects.
    pub fn is_final(self) -> bool {
        matches!(self, Action::LogOut | Action::Restart | Action::ShutDown)
    }
}

/// What logind says this session may actually do.
///
/// Defaults to everything unavailable. A dock that cannot reach logind should
/// grey the actions out rather than offer them and fail on click — the failure
/// would otherwise arrive as nothing happening, which is the worst reading of
/// a Shut Down button.
#[derive(Debug, Clone, Copy, Default)]
pub struct Availability {
    pub suspend: bool,
    pub restart: bool,
    pub shut_down: bool,
    /// Ending your own session needs no capability check — logind lets a
    /// session terminate itself — but it does need to know which session.
    pub log_out: bool,
    /// Requires a display manager that implements the greeter switch.
    pub switch_user: bool,
    /// Always false today. Kept as a field rather than omitted so the menu is
    /// written against the same shape it will have when a locker exists, and
    /// so the disabled item has a reason attached rather than being special.
    pub lock: bool,
}

impl Availability {
    pub fn allows(&self, action: Action) -> bool {
        match action {
            Action::SwitchUser => self.switch_user,
            Action::LogOut => self.log_out,
            Action::Lock => self.lock,
            Action::Suspend => self.suspend,
            Action::Restart => self.restart,
            Action::ShutDown => self.shut_down,
        }
    }

    /// Why an item is greyed out, for its tooltip.
    ///
    /// A disabled control with no explanation is indistinguishable from a
    /// broken one, and this menu disables things for four quite different
    /// reasons.
    pub fn why_not(&self, action: Action) -> Option<&'static str> {
        if self.allows(action) {
            return None;
        }
        Some(match action {
            Action::Lock => "No session locker yet \u{2014} the display manager's lock is\nbypassable by cusk's own VT switching",
            Action::SwitchUser => "The display manager does not offer a greeter switch",
            Action::LogOut => "No logind session to end",
            _ => "logind says this session may not do that",
        })
    }
}

/// `yes` and `challenge` both mean the action is possible.
///
/// `challenge` means logind would ask polkit to authenticate first, which is a
/// prompt rather than a refusal. Treating it as unavailable would grey out
/// shutdown on every multi-session machine.
fn permitted(answer: &str) -> bool {
    matches!(answer, "yes" | "challenge")
}

/// The logind session this process belongs to.
///
/// `XDG_SESSION_ID` rather than asking logind to resolve the caller: the dock
/// is in the session it wants to act on, and reading the variable it was
/// started with is both cheaper and honest about the assumption.
fn session_id() -> Option<String> {
    std::env::var("XDG_SESSION_ID").ok().filter(|s| !s.is_empty())
}

/// The display-manager seat object, as the DM exported it into the environment.
///
/// Set by sddm/lightdm/gdm when they start the session. Absent means the
/// session was started some other way — a bare `cusk --tty` from a VT — and a
/// greeter switch has nowhere to go.
fn seat_path() -> Option<String> {
    std::env::var("XDG_SEAT_PATH").ok().filter(|s| s.starts_with('/'))
}

#[zbus::proxy(
    interface = "org.freedesktop.login1.Manager",
    default_service = "org.freedesktop.login1",
    default_path = "/org/freedesktop/login1"
)]
trait Login1Manager {
    fn suspend(&self, interactive: bool) -> zbus::Result<()>;
    fn reboot(&self, interactive: bool) -> zbus::Result<()>;
    fn power_off(&self, interactive: bool) -> zbus::Result<()>;
    fn terminate_session(&self, session_id: &str) -> zbus::Result<()>;
    fn lock_session(&self, session_id: &str) -> zbus::Result<()>;
    #[zbus(name = "CanSuspend")]
    fn can_suspend(&self) -> zbus::Result<String>;
    #[zbus(name = "CanReboot")]
    fn can_reboot(&self) -> zbus::Result<String>;
    #[zbus(name = "CanPowerOff")]
    fn can_power_off(&self) -> zbus::Result<String>;
}

/// The display manager's seat.
///
/// # Why `Lock` is here and not used
///
/// This interface also exposes `Lock`, and sddm implements it — it switches the
/// seat to a greeter that authenticates before coming back. On most desktops
/// that is a reasonable lock. On this one it is not, and the reason is specific
/// to cusk: cusk implements Ctrl+Alt+F<n> itself (see tty.rs, which had to,
/// because holding logind's session control disables the kernel's own VT
/// switching). So the session stays running and unlocked on its own VT, and the
/// chord that switches back to it still works.
///
/// A lock that a documented key combination walks past is worse than a missing
/// one, because it will be trusted. The real fix is ext-session-lock-v1 in the
/// compositor, where the *session* is locked rather than hidden — smithay 0.7
/// has `wayland::session_lock` for exactly this. Until then Lock stays disabled
/// and says why.
#[zbus::proxy(
    interface = "org.freedesktop.DisplayManager.Seat",
    default_service = "org.freedesktop.DisplayManager"
)]
trait DisplayManagerSeat {
    fn switch_to_greeter(&self) -> zbus::Result<()>;
    /// Whether the display manager can put up a greeter at all. False on a seat
    /// with no spare VT, which is the case a bare `--seat` install hits.
    #[zbus(property)]
    fn can_switch(&self) -> zbus::Result<bool>;
}

/// Ask logind what this session is allowed to do.
///
/// Blocking, and called once during boot rather than from a frame. Every
/// failure degrades to "not available" rather than propagating: the dock has to
/// draw either way, and the alternative to a greyed-out Shut Down is no dock.
pub fn probe() -> Availability {
    let mut available = Availability {
        // Not a logind capability question: logind always lets a session
        // end itself, it just needs to know which one.
        log_out: session_id().is_some(),
        // Provisional. Set from the display manager's own CanSwitch below;
        // having a seat path only means there is something to ask.
        switch_user: false,
        // Nothing implements ext-session-lock-v1 in this tree yet. When
        // something does, this becomes a real check.
        lock: false,
        ..Availability::default()
    };

    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        eprintln!("cusk-dock: no runtime for the logind probe; session actions disabled");
        return available;
    };

    runtime.block_on(async {
        // Bounded, because this runs before the first frame. An unreachable
        // system bus must delay the dock appearing, not prevent it.
        let connect = tokio::time::timeout(Duration::from_secs(2), zbus::Connection::system());
        let connection = match connect.await {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                eprintln!("cusk-dock: no system bus ({e}); session actions disabled");
                return;
            }
            Err(_) => {
                eprintln!("cusk-dock: system bus timed out; session actions disabled");
                return;
            }
        };
        let Ok(manager) = Login1ManagerProxy::new(&connection).await else {
            eprintln!("cusk-dock: logind is not on the bus; session actions disabled");
            return;
        };
        available.suspend = manager.can_suspend().await.is_ok_and(|a| permitted(&a));
        available.restart = manager.can_reboot().await.is_ok_and(|a| permitted(&a));
        available.shut_down = manager.can_power_off().await.is_ok_and(|a| permitted(&a));

        // The display manager is a separate service and a separate
        // question. Ask it rather than inferring from XDG_SEAT_PATH: the
        // variable says a display manager started this session, not that it
        // can put up a greeter now. CanSwitch is false on a seat with no
        // spare VT, and Switch User would then be an enabled item that
        // fails silently.
        if let Some(path) = seat_path() {
            if let Ok(seat) = DisplayManagerSeatProxy::builder(&connection)
                .path(path)
                .map(|b| b.build())
            {
                if let Ok(seat) = seat.await {
                    available.switch_user = seat.can_switch().await.unwrap_or(false);
                }
            }
        }
    });

    available
}

/// Perform an action, on a thread of its own.
///
/// Detached deliberately. `PowerOff` does not return before the machine goes
/// down, and `Suspend` blocks for as long as the machine is asleep — doing
/// either on the UI thread freezes the dock at exactly the moment the user is
/// watching it to see whether their click registered.
pub fn perform(action: Action) {
    std::thread::spawn(move || {
        if let Err(e) = run(action) {
            // A failed session action is not a crash and must not be silent.
            // logind refusing a shutdown because another user is logged in is
            // an ordinary outcome the user needs told about.
            eprintln!("cusk-dock: {} failed: {e}", action.label());
        }
    });
}

fn run(action: Action) -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("no runtime: {e}"))?;

    runtime.block_on(async {
        let connection = zbus::Connection::system()
            .await
            .map_err(|e| format!("no system bus: {e}"))?;

        if action == Action::SwitchUser {
            let path = seat_path().ok_or("XDG_SEAT_PATH is not set")?;
            let seat = DisplayManagerSeatProxy::builder(&connection)
                .path(path)
                .map_err(|e| format!("bad seat path: {e}"))?
                .build()
                .await
                .map_err(|e| format!("no display manager: {e}"))?;
            return seat.switch_to_greeter().await.map_err(|e| e.to_string());
        }

        let manager = Login1ManagerProxy::new(&connection)
            .await
            .map_err(|e| format!("logind unreachable: {e}"))?;

        // `false` is "non-interactive": do not raise a polkit prompt from a
        // click the user has already made. A `challenge` result then comes back
        // as an error rather than as a dialog appearing over the desktop from a
        // process that has no business parenting one.
        match action {
            Action::Suspend => manager.suspend(false).await,
            Action::Restart => manager.reboot(false).await,
            Action::ShutDown => manager.power_off(false).await,
            Action::LogOut => {
                let id = session_id().ok_or("XDG_SESSION_ID is not set")?;
                manager.terminate_session(&id).await
            }
            Action::Lock => {
                let id = session_id().ok_or("XDG_SESSION_ID is not set")?;
                // Emits a Lock signal on the session. Nothing in this tree
                // listens for it yet, which is why the menu item is disabled —
                // the call is here so that installing a locker is the only
                // thing needed to make it work.
                manager.lock_session(&id).await
            }
            Action::SwitchUser => unreachable!("handled above"),
        }
        .map_err(|e| e.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn challenge_is_permitted_because_it_is_a_prompt_not_a_refusal() {
        assert!(permitted("yes"));
        assert!(permitted("challenge"));
        assert!(!permitted("no"));
        // `na` means the hardware or kernel cannot, which is not policy.
        assert!(!permitted("na"));
        assert!(!permitted(""));
    }

    #[test]
    fn nothing_is_offered_when_logind_is_unreachable() {
        let none = Availability::default();
        for action in [
            Action::SwitchUser,
            Action::LogOut,
            Action::Lock,
            Action::Suspend,
            Action::Restart,
            Action::ShutDown,
        ] {
            assert!(!none.allows(action), "{action:?} offered by default");
            assert!(none.why_not(action).is_some(), "{action:?} disabled without a reason");
        }
    }

    #[test]
    fn an_allowed_action_has_no_disabled_reason() {
        let all = Availability {
            suspend: true,
            restart: true,
            shut_down: true,
            log_out: true,
            switch_user: true,
            lock: true,
        };
        assert!(all.why_not(Action::ShutDown).is_none());
        assert!(all.why_not(Action::Lock).is_none());
    }

    #[test]
    fn lock_explains_itself_rather_than_blaming_policy() {
        // The reason Lock is disabled is that this tree has no locker, which is
        // a different sentence from logind refusing — and the one the user can
        // act on.
        let none = Availability::default();
        assert!(none.why_not(Action::Lock).is_some_and(|r| r.contains("No session locker")));
    }

    #[test]
    fn only_the_session_ending_actions_are_final() {
        assert!(Action::LogOut.is_final());
        assert!(Action::Restart.is_final());
        assert!(Action::ShutDown.is_final());
        assert!(!Action::Suspend.is_final());
        assert!(!Action::Lock.is_final());
        assert!(!Action::SwitchUser.is_final());
    }
}
