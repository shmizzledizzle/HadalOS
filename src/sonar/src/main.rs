//! sonar — compute inventory for HadalOS.
//!
//! docs/compute.md §6 step 1: enumerate and print. No dispatch, no policy, no
//! leases. The point of shipping this alone is that §4.3's invariant — no
//! compute lease may cause the compositor to miss a frame — is a guess until
//! the machine has been measured, and the enum it is written against was
//! already wrong once (see cpu.rs).
//!
//! Exits 0 when it produced an inventory, 1 when it could not.

mod cpu;
#[cfg(test)]
mod fixture;
mod gpu;
mod sysfs;

use std::process::ExitCode;

const USAGE: &str = "\
sonar — compute inventory

usage: sonar [--json]

  --json    machine-readable output
  -h        this text
";

fn main() -> ExitCode {
    // Closed match with a named fallback, the same shape as hadald's config.rs.
    // An unrecognised flag is an error rather than being ignored: a typo'd
    // --json that silently prints a table is the class of quiet failure this
    // component was written in reaction to.
    let mut json = false;
    for arg in std::env::args().skip(1) {
        match arg.as_str() {
            "--json" => json = true,
            "-h" | "--help" => {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("sonar: unknown argument: {other}");
                eprint!("{USAGE}");
                return ExitCode::FAILURE;
            }
        }
    }

    let cpus = cpu::inventory();
    let gpus = gpu::inventory();
    let icds = gpu::vulkan_icds();

    // No CPU classes means sysfs was unreadable, which is a failure and not an
    // empty inventory. Every machine has at least one.
    if cpus.classes.is_empty() {
        eprintln!("sonar: could not read /sys/devices/system/cpu — no inventory produced");
        return ExitCode::FAILURE;
    }

    if json {
        println!(
            r#"{{"cpu":{},"gpu":{}}}"#,
            cpu::to_json(&cpus),
            gpu::to_json(&gpus, &icds)
        );
    } else {
        print_human(&cpus, &gpus, &icds);
    }

    ExitCode::SUCCESS
}

fn print_human(cpus: &cpu::CpuInventory, gpus: &[gpu::Gpu], icds: &[String]) {
    println!("CPU");

    if cpus.classes.len() == 1 {
        println!("  1 core class — homogeneous, nothing to route between");
    } else {
        let axes: Vec<&str> = cpus.asymmetry.iter().map(|a| a.as_str()).collect();
        println!(
            "  {} core classes, differing in: {}",
            cpus.classes.len(),
            if axes.is_empty() {
                "nothing detected — check the grouping key".to_string()
            } else {
                axes.join(", ")
            }
        );
    }

    for (i, c) in cpus.classes.iter().enumerate() {
        println!(
            "  class {}  {:<22} {:>5} MHz  L2 {:>5} KiB  L3 {:>6} KiB  SMT {}",
            i,
            c.label(),
            c.max_mhz(),
            c.l2_kb,
            c.l3_kb,
            if c.smt {
                format!("{}x", c.threads_per_core())
            } else {
                "off".to_string()
            }
        );
        println!("            cpus {}", fmt_ranges(&c.cpus));
        // More than one L2 domain in a class is the cache-locality axis: on
        // Intel E-core clusters and on AMD CCXs alike, work split across
        // domains loses the shared cache.
        if c.l2_domains.len() > 1 {
            let d: Vec<String> = c.l2_domains.iter().map(|d| fmt_ranges(d)).collect();
            println!("            L2 domains: {}", d.join("  "));
        }
        // On AMD this is the CCX split. Two same-size CCXs stay one class, so
        // the domains are the only place that locality is visible.
        if c.l3_domains.len() > 1 {
            let d: Vec<String> = c.l3_domains.iter().map(|d| fmt_ranges(d)).collect();
            println!("            L3 domains: {}", d.join("  "));
        }
    }

    if let Some(m) = cpus.mem_total_bytes {
        println!("  system memory {}", sysfs::fmt_bytes(m));
    }

    if cpus.capacity_is_misleading {
        println!(
            "  note: cpu_capacity is uniform across {} classes — do not route on it",
            cpus.classes.len()
        );
    }

    println!();
    println!("GPU");
    if gpus.is_empty() {
        println!("  none found under /sys/class/drm");
    }
    for g in gpus {
        println!(
            "  {:<7} {:<8} {:<10} {}",
            g.card,
            g.driver.as_deref().unwrap_or("?"),
            g.kind(),
            g.memory_note()
        );
        println!(
            "          outputs: {}",
            if g.connected_outputs.is_empty() {
                "none connected".to_string()
            } else {
                g.connected_outputs.join(", ")
            }
        );
        println!(
            "          render node: {}   contends with display: {}",
            if g.has_render_node { "yes" } else { "no" },
            if g.contends_with_display() { "YES" } else { "no" }
        );
    }

    println!();
    println!("Vulkan ICDs installed (installed, not verified loadable)");
    if icds.is_empty() {
        println!("  none");
    }
    for i in icds {
        println!("  {i}");
    }
}

/// `[0,1,2,3,8,9]` → `0-3,8-9`. Inventory output is read by people, and a
/// twelve-element list of integers is not read, it is skipped.
fn fmt_ranges(cpus: &[u32]) -> String {
    if cpus.is_empty() {
        return "-".into();
    }
    let mut parts = Vec::new();
    let mut start = cpus[0];
    let mut prev = cpus[0];
    for &c in &cpus[1..] {
        if c == prev + 1 {
            prev = c;
            continue;
        }
        parts.push(range_str(start, prev));
        start = c;
        prev = c;
    }
    parts.push(range_str(start, prev));
    parts.join(",")
}

fn range_str(a: u32, b: u32) -> String {
    if a == b {
        a.to_string()
    } else {
        format!("{a}-{b}")
    }
}

#[cfg(test)]
mod tests {
    use super::fmt_ranges;

    #[test]
    fn ranges_collapse() {
        assert_eq!(fmt_ranges(&[0, 1, 2, 3]), "0-3");
        assert_eq!(fmt_ranges(&[4]), "4");
        assert_eq!(fmt_ranges(&[0, 1, 2, 3, 8, 9, 10, 11]), "0-3,8-11");
        assert_eq!(fmt_ranges(&[0, 2, 4]), "0,2,4");
        assert_eq!(fmt_ranges(&[]), "-");
    }
}
