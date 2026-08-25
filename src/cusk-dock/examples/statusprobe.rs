//! What the strip would show, printed.
//!
//! `cargo run --example statusprobe`. The readouts are two lines of text on a
//! 26-pixel strip; a wrong one is a wrong number that looks exactly like a
//! right one. This prints what the dock would draw, beside the raw files and
//! properties it came from, so the two can be compared by eye.
/// Every icon name the readouts can ask for, at every state they can reach.
///
/// Checked because the failure is silent: a name this theme does not have
/// leaves a number with no glyph beside it, on a strip where there is nothing
/// to compare it against. Nothing here breaks the dock — it is a missing
/// picture, not a missing readout — but a whole family missing means the
/// theme uses different names and the strip is wrong everywhere at once.
fn icons() {
    let names = [
        "network-offline",
        "network-wired",
        "network-wired-no-route",
        "network-wireless-no-route",
        "network-wireless-signal-none",
        "network-wireless-signal-weak",
        "network-wireless-signal-ok",
        "network-wireless-signal-good",
        "network-wireless-signal-excellent",
        "battery-caution",
        "battery-low",
        "battery-good",
        "battery-full",
        "battery-caution-charging",
        "battery-low-charging",
        "battery-good-charging",
        "battery-full-charging",
    ];
    let mut missing = 0;
    for name in names {
        match cusk::entry::find_icon(name) {
            Some(_) => {}
            None => {
                println!("  absent   {name}");
                missing += 1;
            }
        }
    }
    println!("individual names: {}/{} present", names.len() - missing, names.len());

    // What actually matters is that every *chain* resolves to something. An
    // absent name above is fine if the fallback beside it is present; a chain
    // that resolves to nothing is a readout with no picture.
    let mut chains: Vec<&'static [&'static str]> = Vec::new();
    for state in [
        cusk_dock::battery::Charge::Charging,
        cusk_dock::battery::Charge::Discharging,
        cusk_dock::battery::Charge::Full,
    ] {
        for percent in [5u8, 20, 50, 95] {
            chains.push(
                cusk_dock::battery::Battery {
                    percent,
                    state,
                    remaining: None,
                }
                .icons(),
            );
        }
    }
    use cusk_dock::network::{Link, Network, Reach};
    for reach in [Reach::Full, Reach::Portal, Reach::Limited, Reach::None, Reach::Unknown] {
        for link in [
            Link::Down,
            Link::Wired,
            Link::Other("VPN".to_string()),
            Link::Wireless { ssid: "x".into(), strength: 10 },
            Link::Wireless { ssid: "x".into(), strength: 50 },
            Link::Wireless { ssid: "x".into(), strength: 90 },
        ] {
            chains.push(Network { link, reach }.icons());
        }
    }

    let mut blank = 0;
    for chain in &chains {
        if !chain.iter().any(|name| cusk::entry::find_icon(name).is_some()) {
            println!("  NO GLYPH {chain:?}");
            blank += 1;
        }
    }
    println!("chains: {}/{} draw something", chains.len() - blank, chains.len());
}

/// The name the strip would actually draw: the first of the chain this theme
/// has, or a note that it has none.
fn drawn(chain: &[&str]) -> String {
    chain
        .iter()
        .find(|name| cusk::entry::find_icon(name).is_some())
        .map(|name| name.to_string())
        .unwrap_or_else(|| format!("no glyph, tried {chain:?}"))
}

fn main() {
    icons();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a runtime");
    let mut watcher = cusk_dock::network::Watcher::default();
    let network = runtime.block_on(watcher.read());
    println!("network: {:?}  [{}]", network.label(), drawn(network.icons()));
    println!("         {}", network.detail());
    println!("         link={:?} reach={:?}", network.link, network.reach);

    match cusk_dock::battery::read() {
        None => println!("battery: none (this machine has no system battery)"),
        Some(battery) => {
            println!("battery: {}  [{}]", battery.label(), drawn(battery.icons()));
            println!("         {}", battery.detail());
        }
    }
}
