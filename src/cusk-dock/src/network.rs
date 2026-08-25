//! Whether there is a network, and what kind.
//!
//! NetworkManager over D-Bus, with a sysfs fallback. The opposite choice from
//! `battery`, and for the opposite reason: the interesting facts here are ones
//! sysfs does not have. `/sys/class/net` can say a link is up; it cannot say
//! which network it is on, how strong the signal is, or — the one that matters
//! most — whether packets actually reach the internet. A dock showing a full
//! wifi icon on a captive portal is telling you the opposite of what you need
//! to know.
//!
//! So NetworkManager when it is there, which on HadalOS it is, and sysfs when
//! it is not, which is honest about knowing less rather than pretending.
//!
//! # Connectivity is not the same as connected
//!
//! `Connectivity` is NetworkManager's own answer to "did an HTTP probe get
//! through", not "is a cable in". They differ exactly when it matters: a hotel
//! wifi that has associated and assigned an address but intercepts every
//! request reports `Portal`, and every other indicator on the machine says the
//! network is fine. Keeping the two separate — `link` for what is attached,
//! `reach` for whether it goes anywhere — is what lets the strip say so.

use std::time::Duration;

/// What is carrying the traffic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Link {
    Wireless {
        ssid: String,
        /// Percent, as NetworkManager reports it.
        strength: u8,
    },
    Wired,
    /// A connection of some other type — a VPN on its own, a mobile broadband
    /// dongle, a bridge. Named rather than folded into `Wired`, because
    /// "ethernet" on a machine with no cable in it is a confusing thing to
    /// read.
    Other(String),
    Down,
}

/// Whether traffic gets anywhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    Full,
    /// Associated and addressed, but a captive portal is intercepting.
    Portal,
    /// On a network that does not route to the internet.
    Limited,
    None,
    /// Not asked, or nothing to ask. What the sysfs fallback always reports,
    /// because sysfs genuinely does not know.
    Unknown,
}

impl Reach {
    /// NetworkManager's `NM_CONNECTIVITY_*`.
    fn parse(value: u32) -> Reach {
        match value {
            1 => Reach::None,
            2 => Reach::Portal,
            3 => Reach::Limited,
            4 => Reach::Full,
            _ => Reach::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Network {
    pub link: Link,
    pub reach: Reach,
}

impl Network {
    /// Nothing attached, which is also what every failure here degrades to.
    pub fn offline() -> Network {
        Network {
            link: Link::Down,
            reach: Reach::None,
        }
    }

    /// The full sentence, for the tooltip.
    pub fn detail(&self) -> String {
        let what = match &self.link {
            Link::Wireless { ssid, strength } => format!("{ssid} — {strength}%"),
            Link::Wired => "Wired".to_string(),
            Link::Other(kind) => kind.clone(),
            Link::Down => return "No network".to_string(),
        };
        // The qualifier is only added when it says something. "Wired
        // (connected)" is noise; "Wired (no internet)" is the whole message.
        match self.reach {
            Reach::Full | Reach::Unknown => what,
            Reach::Portal => format!("{what} — sign-in required"),
            Reach::Limited => format!("{what} — no internet"),
            Reach::None => format!("{what} — not connected"),
        }
    }

    /// Icon names to ask the theme for, best first.
    ///
    /// A list rather than one name, because freedesktop icon naming is a
    /// convention that themes follow unevenly and a single hardcoded name is
    /// how this readout shipped with no glyph the first time. `-no-route` is
    /// the name the specification suggests for a link that carries nothing and
    /// exists in no theme on the reference machine; breeze spells the same
    /// idea `network-limited`. Both are listed, and the first one present
    /// wins.
    ///
    /// The last entry of every chain is one that resolves on a bare hicolor
    /// install, so the fallback is a less specific picture rather than none.
    pub fn icons(&self) -> &'static [&'static str] {
        // A link that carries nothing is drawn as a problem regardless of how
        // strong its signal is. Full bars on a captive portal is the single
        // most misleading thing this indicator could show.
        let broken = matches!(self.reach, Reach::Portal | Reach::Limited | Reach::None);
        match &self.link {
            Link::Down => &["network-offline", "network-unavailable", "network-wired"],
            Link::Wireless { strength, .. } => {
                if broken {
                    return &[
                        "network-wireless-no-route",
                        "network-limited",
                        "network-wireless-disconnected",
                        "network-wireless",
                    ];
                }
                match strength {
                    0..=20 => &["network-wireless-signal-none", "network-wireless"],
                    21..=40 => &["network-wireless-signal-weak", "network-wireless"],
                    41..=60 => &["network-wireless-signal-ok", "network-wireless"],
                    61..=80 => &["network-wireless-signal-good", "network-wireless"],
                    _ => &["network-wireless-signal-excellent", "network-wireless"],
                }
            }
            Link::Wired | Link::Other(_) => {
                if broken {
                    &["network-wired-no-route", "network-limited", "network-wired"]
                } else {
                    &["network-wired"]
                }
            }
        }
    }

    /// The short label beside the icon on the strip.
    ///
    /// Signal percent for wifi and nothing for anything else. A wired link has
    /// no number worth two characters of a 26-pixel strip: it is either up or
    /// it is not, and the icon already says which.
    pub fn label(&self) -> Option<String> {
        match &self.link {
            Link::Wireless { strength, .. } => Some(format!("{strength}%")),
            _ => None,
        }
    }
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager",
    default_service = "org.freedesktop.NetworkManager",
    default_path = "/org/freedesktop/NetworkManager"
)]
trait Manager {
    #[zbus(property)]
    fn connectivity(&self) -> zbus::Result<u32>;
    #[zbus(property)]
    fn primary_connection(&self) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Connection.Active",
    default_service = "org.freedesktop.NetworkManager"
)]
trait ActiveConnection {
    #[zbus(property)]
    fn type_(&self) -> zbus::Result<String>;
    #[zbus(property)]
    fn devices(&self) -> zbus::Result<Vec<zbus::zvariant::OwnedObjectPath>>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.Device.Wireless",
    default_service = "org.freedesktop.NetworkManager"
)]
trait WirelessDevice {
    #[zbus(property)]
    fn active_access_point(&self) -> zbus::Result<zbus::zvariant::OwnedObjectPath>;
}

#[zbus::proxy(
    interface = "org.freedesktop.NetworkManager.AccessPoint",
    default_service = "org.freedesktop.NetworkManager"
)]
trait AccessPoint {
    /// Bytes, not a string. An SSID is an opaque 32-byte identifier and is not
    /// required to be UTF-8; NetworkManager passes it through unchanged, which
    /// is why this is `ay` and why `ssid_text` below has to make a decision
    /// about what to do with one that is not.
    #[zbus(property)]
    fn ssid(&self) -> zbus::Result<Vec<u8>>;
    #[zbus(property)]
    fn strength(&self) -> zbus::Result<u8>;
}

/// A held system-bus connection, and the polling built on it.
///
/// The connection is opened once and kept, not rebuilt per poll. Opening one
/// is a socket, an authentication handshake and a `Hello`, which is a great
/// deal of work to repeat every two seconds to answer a question whose answer
/// almost never changes.
///
/// Keeping it is also correct across a NetworkManager restart: proxies resolve
/// the service by name on each call, so a new NetworkManager is picked up
/// without reconnecting to the bus. Only the bus itself going away would
/// invalidate this, and that takes the session with it.
#[derive(Debug, Default)]
pub struct Watcher {
    connection: Option<zbus::Connection>,
}

impl Watcher {
    /// Read the current state.
    ///
    /// Every D-Bus step is allowed to fail into the sysfs fallback rather than
    /// into nothing: a machine where NetworkManager is not the network manager
    /// still has interfaces, and reporting "no network" on a working
    /// statically configured box would be wrong in the most annoying possible
    /// way.
    pub async fn read(&mut self) -> Network {
        match self.from_manager().await {
            Some(network) => network,
            None => from_sysfs(),
        }
    }

    /// The bus, connecting on first use and on every use after a failure.
    ///
    /// Retried rather than given up on, because the dock can start before
    /// NetworkManager does — the strip would otherwise show sysfs's answer for
    /// the rest of the session on a machine that has a perfectly good one.
    async fn bus(&mut self) -> Option<&zbus::Connection> {
        if self.connection.is_none() {
            // Bounded, because the point of doing this on a background thread
            // is that the strip keeps drawing. A call to a service that is
            // still starting can block for its activation timeout, which is
            // much longer than a poll interval.
            let attempt = tokio::time::timeout(Duration::from_secs(2), zbus::Connection::system());
            self.connection = attempt.await.ok().and_then(|r| r.ok());
        }
        self.connection.as_ref()
    }

    async fn from_manager(&mut self) -> Option<Network> {
        let connection = self.bus().await?.clone();
        let manager = ManagerProxy::new(&connection).await.ok()?;

        let reach = Reach::parse(manager.connectivity().await.ok()?);
        let primary = manager.primary_connection().await.ok()?;

        // "/" is NetworkManager's null path, and it is what a machine with
        // nothing connected reports. Not an error — the answer is "down".
        if primary.as_str() == "/" {
            return Some(Network {
                link: Link::Down,
                reach: Reach::None,
            });
        }

        let active = ActiveConnectionProxy::builder(&connection)
            .path(primary)
            .ok()?
            .build()
            .await
            .ok()?;
        let kind = active.type_().await.ok()?;

        let link = match kind.as_str() {
            "802-11-wireless" => wireless(&connection, &active).await.unwrap_or(Link::Other(
                // Associated but the access point could not be read — which
                // happens for a moment during a roam. Better than claiming a
                // signal strength that was not measured.
                "Wi-Fi".to_string(),
            )),
            "802-3-ethernet" => Link::Wired,
            other => Link::Other(pretty(other)),
        };

        Some(Network { link, reach })
    }
}

async fn wireless(
    connection: &zbus::Connection,
    active: &ActiveConnectionProxy<'_>,
) -> Option<Link> {
    let devices = active.devices().await.ok()?;
    let device = devices.into_iter().next()?;
    let wifi = WirelessDeviceProxy::builder(connection)
        .path(device)
        .ok()?
        .build()
        .await
        .ok()?;
    let ap_path = wifi.active_access_point().await.ok()?;
    if ap_path.as_str() == "/" {
        return None;
    }
    let ap = AccessPointProxy::builder(connection)
        .path(ap_path)
        .ok()?
        .build()
        .await
        .ok()?;
    Some(Link::Wireless {
        ssid: ssid_text(&ap.ssid().await.ok()?),
        strength: ap.strength().await.ok()?.min(100),
    })
}

/// Turn NetworkManager's SSID bytes into something drawable.
///
/// An SSID is 32 opaque bytes. Most are UTF-8; some are Latin-1, some are
/// deliberately not text at all, and a hidden network's is empty. Every one of
/// those has to render as *something* — a tooltip that is blank, or that panics
/// the dock, is worse than one naming a network oddly.
fn ssid_text(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return "Hidden network".to_string();
    }
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_string(),
        // Lossy rather than refused. The replacement characters make it
        // visibly odd, which is accurate, and the rest of the name still
        // reads.
        Err(_) => String::from_utf8_lossy(bytes).into_owned(),
    }
}

/// A connection type NetworkManager names in its own vocabulary, made
/// readable. Anything unrecognised is passed through rather than hidden — an
/// unfamiliar word is more use than "Other".
fn pretty(kind: &str) -> String {
    match kind {
        "gsm" | "cdma" => "Mobile".to_string(),
        "vpn" | "wireguard" => "VPN".to_string(),
        "bridge" => "Bridge".to_string(),
        "bond" => "Bond".to_string(),
        "tun" => "Tunnel".to_string(),
        other => other.to_string(),
    }
}

/// What can be known without NetworkManager.
///
/// Up or down, wired or wireless, and nothing about whether traffic arrives —
/// which is reported as `Unknown` rather than guessed at. `lo` is skipped: it
/// is always up and would make a disconnected machine claim a network.
fn from_sysfs() -> Network {
    let Ok(entries) = std::fs::read_dir("/sys/class/net") else {
        return Network::offline();
    };

    let mut wired = false;
    for path in entries.flatten().map(|e| e.path()) {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "lo" || name.is_empty() {
            continue;
        }
        // `operstate` and not `carrier`: carrier says a cable is plugged in,
        // operstate says the interface is actually running. A cable into an
        // unconfigured interface is not a network.
        let up = std::fs::read_to_string(path.join("operstate"))
            .map(|s| s.trim() == "up")
            .unwrap_or(false);
        if !up {
            continue;
        }
        // The `wireless` directory is how the kernel marks a wifi interface.
        // Name prefixes — `wl`, `wlan` — are convention and predictable naming
        // has already changed them once.
        if path.join("wireless").is_dir() {
            return Network {
                // No strength: `/proc/net/wireless` has one, in units that
                // need the driver's maximum to interpret, and a percentage
                // derived from a guess at that is a number that looks precise
                // and is not.
                link: Link::Wireless {
                    ssid: "Wi-Fi".to_string(),
                    strength: 0,
                },
                reach: Reach::Unknown,
            };
        }
        wired = true;
    }

    if wired {
        Network {
            link: Link::Wired,
            reach: Reach::Unknown,
        }
    } else {
        Network::offline()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wifi(strength: u8, reach: Reach) -> Network {
        Network {
            link: Link::Wireless {
                ssid: "trench".to_string(),
                strength,
            },
            reach,
        }
    }

    #[test]
    fn connectivity_values_map_to_the_states_that_matter() {
        assert_eq!(Reach::parse(4), Reach::Full);
        assert_eq!(Reach::parse(2), Reach::Portal);
        assert_eq!(Reach::parse(3), Reach::Limited);
        assert_eq!(Reach::parse(1), Reach::None);
        assert_eq!(Reach::parse(0), Reach::Unknown);
        // A value from a NetworkManager newer than this build. Unknown, not a
        // panic and not silently Full.
        assert_eq!(Reach::parse(99), Reach::Unknown);
    }

    #[test]
    fn a_captive_portal_never_shows_full_bars() {
        // The failure this indicator exists to avoid. Signal is excellent and
        // nothing works; an icon chosen on strength alone would say everything
        // is fine.
        assert_eq!(wifi(100, Reach::Portal).icons()[0], "network-wireless-no-route");
        assert_eq!(wifi(100, Reach::Limited).icons()[0], "network-wireless-no-route");
        assert_eq!(
            wifi(100, Reach::Full).icons()[0],
            "network-wireless-signal-excellent"
        );
    }

    #[test]
    fn a_portal_says_what_to_do_about_it() {
        assert_eq!(wifi(80, Reach::Portal).detail(), "trench — 80% — sign-in required");
        assert_eq!(wifi(80, Reach::Limited).detail(), "trench — 80% — no internet");
    }

    #[test]
    fn a_working_connection_is_not_annotated() {
        // "Wired (connected)" is noise. The qualifier only earns its place
        // when it changes what you would do.
        assert_eq!(wifi(80, Reach::Full).detail(), "trench — 80%");
        let wired = Network {
            link: Link::Wired,
            reach: Reach::Full,
        };
        assert_eq!(wired.detail(), "Wired");
    }

    #[test]
    fn an_unknown_reach_is_not_reported_as_a_fault() {
        // What the sysfs fallback always returns. It means "not asked", and
        // drawing it as a problem would put a warning on every machine
        // without NetworkManager.
        let wired = Network {
            link: Link::Wired,
            reach: Reach::Unknown,
        };
        assert_eq!(wired.detail(), "Wired");
        assert_eq!(wired.icons(), &["network-wired"]);
    }

    #[test]
    fn offline_says_so_without_a_qualifier() {
        assert_eq!(Network::offline().detail(), "No network");
        assert_eq!(Network::offline().icons()[0], "network-offline");
        assert_eq!(Network::offline().label(), None);
    }

    #[test]
    fn only_wireless_carries_a_number_on_the_strip() {
        assert_eq!(wifi(64, Reach::Full).label().as_deref(), Some("64%"));
        let wired = Network {
            link: Link::Wired,
            reach: Reach::Full,
        };
        assert_eq!(wired.label(), None);
    }

    #[test]
    fn signal_bands_span_the_whole_range() {
        // No gap and no overlap: every strength from 0 to 100 names an icon.
        for strength in 0..=100u8 {
            let icons = wifi(strength, Reach::Full).icons();
            assert!(icons[0].starts_with("network-wireless-signal-"), "{strength}");
            // And every chain ends somewhere generic, so a theme without the
            // graded names still draws something.
            assert_eq!(icons.last(), Some(&"network-wireless"), "{strength}");
        }
    }

    #[test]
    fn a_utf8_ssid_survives_intact() {
        assert_eq!(ssid_text("café-2.4".as_bytes()), "café-2.4");
    }

    #[test]
    fn a_hidden_network_is_named_rather_than_blank() {
        assert_eq!(ssid_text(&[]), "Hidden network");
    }

    #[test]
    fn a_non_utf8_ssid_still_renders() {
        // Latin-1 in the wild. It must not panic and must not come back empty
        // — a blank tooltip reads as a broken dock.
        let out = ssid_text(&[b'c', b'a', b'f', 0xE9]);
        assert!(out.starts_with("caf"));
        assert!(!out.is_empty());
    }

    #[test]
    fn unfamiliar_connection_types_are_shown_rather_than_hidden() {
        assert_eq!(pretty("gsm"), "Mobile");
        assert_eq!(pretty("wireguard"), "VPN");
        // A type this build has never heard of. Its own name is more use than
        // the word "Other".
        assert_eq!(pretty("infiniband"), "infiniband");
    }

    /// Reads whatever this machine has. Asserts only what holds everywhere,
    /// including on a box with no NetworkManager.
    #[test]
    fn the_real_machine_reads_without_panicking() {
        let network = from_sysfs();
        assert!(!network.detail().is_empty());
        assert!(!network.icons().is_empty());
    }
}
