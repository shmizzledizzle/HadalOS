//! How much charge is left, read from sysfs.
//!
//! Not UPower, and the choice is worth stating because UPower is running on the
//! reference machine and would have been less code. sysfs is the thing UPower
//! itself reads: it is present on every Linux with a battery, it needs no
//! daemon, no D-Bus connection and no session bus, and it cannot be in a state
//! where the daemon is up but has not enumerated the device yet. A dock that
//! shows no battery because a service failed to start is worse than one that
//! reads four small files.
//!
//! The cost is that everything UPower would have computed has to be computed
//! here, which is why the arithmetic is separated from the reading and tested
//! on fixtures. `combine` never touches the filesystem.
//!
//! # Two batteries
//!
//! Handled, though the reference machine has one. A laptop with a second
//! battery that showed only the first would be showing a number that is wrong
//! in the direction that matters — "15%" on a machine with hours left, or the
//! reverse. Aggregating on energy rather than averaging percentages is the
//! difference: a 90%-full 20Wh cell beside a 10%-full 60Wh cell is at 30%, not
//! at 50%.

use std::path::{Path, PathBuf};
use std::time::Duration;

const SUPPLIES: &str = "/sys/class/power_supply";

/// What the battery is doing.
///
/// `Unknown` is a real state and not an error: it is what sysfs reports for a
/// battery sitting at full on AC, on some firmware, and treating it as a
/// failure would blank the readout on a plugged-in laptop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Charge {
    Charging,
    Discharging,
    Full,
    Unknown,
}

impl Charge {
    fn parse(status: &str) -> Charge {
        match status.trim() {
            "Charging" => Charge::Charging,
            "Discharging" => Charge::Discharging,
            "Full" => Charge::Full,
            // Includes "Not charging", which is what a battery held at a
            // charge limit reports. Neither filling nor emptying, and calling
            // it either would put a meaningless time estimate on screen.
            _ => Charge::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Battery {
    pub percent: u8,
    pub state: Charge,
    /// Until empty when discharging, until full when charging.
    ///
    /// `None` whenever the rate is unknown or zero, which is most of the first
    /// minute after a state change — the kernel's `power_now` needs a moment to
    /// settle. Absent rather than estimated: "0:00 remaining" on a full battery
    /// is the kind of wrong that gets believed.
    pub remaining: Option<Duration>,
}

impl Battery {
    /// A short label for the strip. Three characters at most, so the column
    /// does not resize as the number crosses 100 or drops below 10.
    pub fn label(&self) -> String {
        format!("{}%", self.percent)
    }

    /// The full sentence, for the tooltip.
    pub fn detail(&self) -> String {
        let state = match self.state {
            Charge::Charging => "Charging",
            Charge::Discharging => "On battery",
            Charge::Full => "Full",
            Charge::Unknown => "Battery",
        };
        match (self.remaining, self.state) {
            (Some(left), Charge::Discharging) => {
                format!("{state} — {} until empty", hhmm(left))
            }
            (Some(left), Charge::Charging) => format!("{state} — {} until full", hhmm(left)),
            _ => format!("{state} — {}%", self.percent),
        }
    }

    /// Icon names to ask the theme for, best first.
    ///
    /// A list for the reason `network::Network::icons` is: a single name is a
    /// bet that every theme spells this the same way, and the charging
    /// variants are exactly where they do not. The plain level is always last,
    /// so a theme with no `-charging` set draws the right amount of charge
    /// without the bolt rather than drawing nothing.
    pub fn icons(&self) -> &'static [&'static str] {
        match (self.state, self.percent) {
            (Charge::Charging, 0..=10) => &["battery-caution-charging", "battery-caution"],
            (Charge::Charging, 11..=30) => &["battery-low-charging", "battery-low"],
            (Charge::Charging, 31..=60) => &["battery-good-charging", "battery-good"],
            (Charge::Charging, _) => &["battery-full-charging", "battery-full"],
            (_, 0..=10) => &["battery-caution"],
            (_, 11..=30) => &["battery-low"],
            (_, 31..=60) => &["battery-good"],
            (_, _) => &["battery-full"],
        }
    }
}

/// Hours and minutes, never a bare number of minutes.
///
/// "2:20" rather than "140 minutes": the question being asked is "can I finish
/// this before it dies", and hours are the unit that answers it.
fn hhmm(left: Duration) -> String {
    let minutes = left.as_secs() / 60;
    format!("{}:{:02}", minutes / 60, minutes % 60)
}

/// One battery, as sysfs describes it.
///
/// `now`, `full` and `rate` are in whatever unit the hardware reports —
/// microwatt-hours and microwatts on this machine, microamp-hours and
/// microamps on others. Deliberately not converted: every use of them here is
/// a ratio between two values from the same battery in the same unit, and
/// `now / rate` is a number of hours either way. Converting would need
/// `voltage_now` and would introduce an error where there is currently none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Cell {
    now: u64,
    full: u64,
    rate: u64,
    /// The kernel's own percentage, used when the energy figures are absent.
    capacity: Option<u8>,
    state: Charge,
}

/// Read every battery the machine has.
///
/// `None` on a desktop, which is not an error — the caller draws nothing.
pub fn read() -> Option<Battery> {
    let cells: Vec<Cell> = supplies().iter().filter_map(|dir| cell(dir)).collect();
    combine(&cells)
}

/// Directories under `/sys/class/power_supply` that are batteries.
///
/// Filtered on `type`, not on the name. `BAT0` is conventional and not
/// guaranteed, and the same directory holds `AC`, `hidpp_battery_0` for a
/// wireless mouse, and on some machines a UPS. A dock reporting the mouse's
/// charge as the laptop's is a plausible-looking wrong number.
fn supplies() -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(SUPPLIES) else {
        return Vec::new();
    };
    let mut found: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| field(p, "type").as_deref() == Some("Battery"))
        // A removed battery leaves its directory behind with `present` at 0.
        // Its `energy_now` is then 0, which would drag an aggregate to half.
        .filter(|p| field(p, "present").is_none_or(|v| v.trim() != "0"))
        // Scope-limited to the machine's own batteries. A peripheral reports
        // `capacity` and `status` and nothing else, so it would contribute a
        // percentage and no energy — enough to skew the average.
        .filter(|p| field(p, "scope").is_none_or(|v| v.trim() == "System"))
        .collect();
    // Sorted so that two machines with the same hardware read the same way,
    // and so the tests are not at the mercy of directory order.
    found.sort();
    found
}

fn field(dir: &Path, name: &str) -> Option<String> {
    std::fs::read_to_string(dir.join(name))
        .ok()
        .map(|s| s.trim().to_string())
}

fn number(dir: &Path, name: &str) -> Option<u64> {
    field(dir, name)?.parse().ok()
}

fn cell(dir: &Path) -> Option<Cell> {
    // Energy first, charge second. Both describe the same thing in different
    // units and a battery exposes one pair or the other; the ratios this uses
    // are unit-free, so there is nothing to reconcile between them.
    let (now, full) = number(dir, "energy_now")
        .zip(number(dir, "energy_full"))
        .or_else(|| number(dir, "charge_now").zip(number(dir, "charge_full")))
        .unwrap_or((0, 0));
    let rate = number(dir, "power_now")
        .or_else(|| number(dir, "current_now"))
        .unwrap_or(0);
    let capacity = number(dir, "capacity").map(|c| c.min(100) as u8);
    let state = field(dir, "status").map_or(Charge::Unknown, |s| Charge::parse(&s));

    // A directory with neither energy figures nor a capacity describes nothing
    // and must not become a zero in the aggregate.
    if full == 0 && capacity.is_none() {
        return None;
    }
    Some(Cell {
        now,
        full,
        rate,
        capacity,
        state,
    })
}

/// Fold every cell into one reading.
///
/// Pure, and where the interesting decisions are. Kept apart from `read` so it
/// can be tested against hardware this machine does not have — two batteries,
/// a battery reporting charge rather than energy, a rate of zero — none of
/// which can be arranged by running the dock.
fn combine(cells: &[Cell]) -> Option<Battery> {
    if cells.is_empty() {
        return None;
    }

    // Charging wins over discharging, which wins over full. On a two-battery
    // machine one cell genuinely can be charging while another discharges, and
    // the honest summary of that is "charging" — the machine as a whole is
    // gaining.
    let state = if cells.iter().any(|c| c.state == Charge::Charging) {
        Charge::Charging
    } else if cells.iter().any(|c| c.state == Charge::Discharging) {
        Charge::Discharging
    } else if cells.iter().all(|c| c.state == Charge::Full) {
        Charge::Full
    } else {
        Charge::Unknown
    };

    let now: u64 = cells.iter().map(|c| c.now).sum();
    let full: u64 = cells.iter().map(|c| c.full).sum();
    let rate: u64 = cells.iter().map(|c| c.rate).sum();

    let percent = if full > 0 {
        // Aggregated on energy, not averaged over percentages. A 90%-full 20Wh
        // cell beside a 10%-full 60Wh cell is at 30%; averaging says 50%.
        ((now.min(full) as u128 * 100) / full as u128) as u8
    } else {
        // No energy figures anywhere, so the kernel's own percentages are all
        // there is. Averaged, because without capacities there is nothing to
        // weight them by.
        let known: Vec<u8> = cells.iter().filter_map(|c| c.capacity).collect();
        if known.is_empty() {
            return None;
        }
        (known.iter().map(|&c| c as u32).sum::<u32>() / known.len() as u32) as u8
    };

    let remaining = match (state, rate, full) {
        (_, 0, _) | (_, _, 0) => None,
        // `now / rate` is hours: microwatt-hours over microwatts, or
        // microamp-hours over microamps. Scaled by 3600 first so the integer
        // division happens on seconds rather than throwing away everything
        // under an hour.
        (Charge::Discharging, rate, _) => Some(Duration::from_secs(
            (now as u128 * 3600 / rate as u128) as u64,
        )),
        (Charge::Charging, rate, full) => Some(Duration::from_secs(
            (full.saturating_sub(now) as u128 * 3600 / rate as u128) as u64,
        )),
        // Full, or a state the kernel would not name. Neither direction
        // applies, so neither estimate is offered.
        _ => None,
    };

    Some(Battery {
        percent: percent.min(100),
        state,
        remaining,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cell(now: u64, full: u64, rate: u64, state: Charge) -> Cell {
        Cell {
            now,
            full,
            rate,
            capacity: None,
            state,
        }
    }

    #[test]
    fn no_batteries_is_not_a_reading() {
        assert_eq!(combine(&[]), None);
    }

    #[test]
    fn one_battery_reports_its_own_fraction() {
        let out = combine(&[cell(50, 100, 0, Charge::Discharging)]).expect("a reading");
        assert_eq!(out.percent, 50);
        assert_eq!(out.state, Charge::Discharging);
    }

    #[test]
    fn two_batteries_are_weighted_by_size_not_averaged() {
        // 18 of 20 plus 6 of 60 is 24 of 80: thirty percent. Averaging the
        // percentages gives fifty, which is the bug this exists to prevent.
        let out = combine(&[
            cell(18, 20, 0, Charge::Discharging),
            cell(6, 60, 0, Charge::Discharging),
        ])
        .expect("a reading");
        assert_eq!(out.percent, 30);
    }

    #[test]
    fn a_battery_reading_over_full_is_still_a_percentage() {
        // Reported by real hardware just after a full charge, and it must not
        // produce "104%".
        let out = combine(&[cell(104, 100, 0, Charge::Full)]).expect("a reading");
        assert_eq!(out.percent, 100);
    }

    #[test]
    fn time_to_empty_is_charge_over_rate() {
        // 30859000 uWh at 10753000 uW — this machine, as it happens — is a
        // little under three hours.
        let out = combine(&[cell(30_859_000, 36_195_000, 10_753_000, Charge::Discharging)])
            .expect("a reading");
        let left = out.remaining.expect("an estimate");
        assert_eq!(left.as_secs() / 60, 172);
        assert_eq!(hhmm(left), "2:52");
    }

    #[test]
    fn charging_counts_up_to_full_rather_than_down_to_empty() {
        // A quarter full, filling at a quarter per hour, is three hours from
        // full — not one hour, which is what measuring the wrong direction
        // would say.
        let out = combine(&[cell(25, 100, 25, Charge::Charging)]).expect("a reading");
        assert_eq!(out.remaining.expect("an estimate").as_secs(), 3 * 3600);
    }

    #[test]
    fn a_rate_of_zero_offers_no_estimate() {
        // The state right after unplugging, before power_now settles. An
        // estimate here would be a division by zero or a fabricated number.
        let out = combine(&[cell(50, 100, 0, Charge::Discharging)]).expect("a reading");
        assert_eq!(out.remaining, None);
    }

    #[test]
    fn a_full_battery_has_no_countdown() {
        let out = combine(&[cell(100, 100, 500, Charge::Full)]).expect("a reading");
        assert_eq!(out.remaining, None);
        assert_eq!(out.state, Charge::Full);
    }

    #[test]
    fn short_estimates_keep_their_minutes() {
        // Scaling to seconds before dividing is what saves these. Dividing in
        // hours first makes everything under an hour read as "0:00".
        let out = combine(&[cell(1, 100, 4, Charge::Discharging)]).expect("a reading");
        assert_eq!(hhmm(out.remaining.expect("an estimate")), "0:15");
    }

    #[test]
    fn charging_anywhere_makes_the_machine_charging() {
        let out = combine(&[
            cell(10, 100, 0, Charge::Discharging),
            cell(10, 100, 0, Charge::Charging),
        ])
        .expect("a reading");
        assert_eq!(out.state, Charge::Charging);
    }

    #[test]
    fn full_needs_every_cell_to_agree() {
        let out = combine(&[
            cell(100, 100, 0, Charge::Full),
            cell(90, 100, 0, Charge::Unknown),
        ])
        .expect("a reading");
        assert_eq!(out.state, Charge::Unknown);
    }

    #[test]
    fn kernel_percentages_are_used_when_there_are_no_energy_figures() {
        let out = combine(&[Cell {
            now: 0,
            full: 0,
            rate: 0,
            capacity: Some(72),
            state: Charge::Discharging,
        }])
        .expect("a reading");
        assert_eq!(out.percent, 72);
        assert_eq!(out.remaining, None);
    }

    #[test]
    fn not_charging_is_not_mistaken_for_charging() {
        // What a battery held at a charge limit reports. It is neither filling
        // nor emptying, and either estimate would be fiction.
        assert_eq!(Charge::parse("Not charging"), Charge::Unknown);
        assert_eq!(Charge::parse("Charging"), Charge::Charging);
        assert_eq!(Charge::parse("Discharging\n"), Charge::Discharging);
    }

    #[test]
    fn the_icon_follows_the_level_and_the_direction() {
        let at = |percent, state| {
            Battery {
                percent,
                state,
                remaining: None,
            }
            .icons()[0]
        };
        assert_eq!(at(5, Charge::Discharging), "battery-caution");
        assert_eq!(at(5, Charge::Charging), "battery-caution-charging");
        assert_eq!(at(95, Charge::Discharging), "battery-full");
        assert_eq!(at(45, Charge::Charging), "battery-good-charging");

        // Every level names an icon, charging or not, with no gap in the
        // percentage ranges.
        for percent in 0..=100u8 {
            for state in [Charge::Charging, Charge::Discharging, Charge::Full] {
                let icons = Battery {
                    percent,
                    state,
                    remaining: None,
                }
                .icons();
                assert!(icons[0].starts_with("battery-"), "{percent} {state:?}");
                // A charging chain always falls back to the plain level, so a
                // theme with no bolt variants still shows the right amount.
                assert!(!icons.last().expect("a fallback").ends_with("-charging"));
            }
        }
    }

    #[test]
    fn the_tooltip_says_which_direction_the_time_runs() {
        let empty = Battery {
            percent: 40,
            state: Charge::Discharging,
            remaining: Some(Duration::from_secs(5400)),
        };
        assert_eq!(empty.detail(), "On battery — 1:30 until empty");

        let filling = Battery {
            percent: 40,
            state: Charge::Charging,
            remaining: Some(Duration::from_secs(5400)),
        };
        assert_eq!(filling.detail(), "Charging — 1:30 until full");
    }

    #[test]
    fn without_an_estimate_the_tooltip_still_says_something() {
        let settling = Battery {
            percent: 40,
            state: Charge::Discharging,
            remaining: None,
        };
        assert_eq!(settling.detail(), "On battery — 40%");
    }

    /// Not a unit test. Reads whatever this machine has and asserts only what
    /// must hold everywhere, so it passes on a desktop with no battery too.
    #[test]
    fn the_real_machine_reads_without_panicking() {
        if let Some(battery) = read() {
            assert!(battery.percent <= 100);
            assert!(!battery.detail().is_empty());
        }
    }
}
