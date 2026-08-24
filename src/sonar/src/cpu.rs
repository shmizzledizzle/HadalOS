//! CPU inventory: core classes and cache domains.
//!
//! # Why not `cpu_capacity`
//!
//! The obvious way to detect a heterogeneous CPU is
//! `/sys/devices/system/cpu/cpu*/cpu_capacity`, and on the reference laptop
//! (i5-1235U, 2 P-cores + 8 E-cores) **every CPU reports 1024**. The attribute
//! is populated by the arm64 topology code; on x86 hybrid it is uniform. Code
//! that trusts it reports a hybrid CPU as homogeneous, and reports it
//! confidently — a plausible, wrong, uniform inventory with nothing to
//! indicate that a scheduling decision was made on a constant.
//!
//! `/sys/devices/system/cpu/types/` (`intel_core` / `intel_atom`) would be the
//! clean discriminator and **does not exist on this kernel** either.
//!
//! So classification is derived from attributes that do vary, all three of
//! which split correctly on this machine:
//!
//! | | P-cores (cpu0-3) | E-cores (cpu4-11) |
//! |---|---|---|
//! | `cpufreq/cpuinfo_max_freq` | 4400000 | 3300000 |
//! | SMT (`topology/core_cpus_list`) | 2 threads | 1 thread |
//! | L2 | 1280K private per core | 2048K shared per 4 |
//!
//! # Why "class", not "CCX"
//!
//! docs/compute.md §4.1 sketched `CpuCcx { id, cores, l3_mb }`. That is
//! AMD-shaped: on a 9800X3D the asymmetry is *cache* at equal frequency. Here
//! it is frequency and SMT at similar cache. A type that names one vendor's
//! mechanism cannot hold the other's, so the unit is a **core class** — a set
//! of CPUs interchangeable with each other and not with the rest — and the
//! *axis* on which classes differ is reported rather than assumed.

use crate::sysfs::{self, parse_cache_kb, parse_cpulist, read, read_u64};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

pub const CPU_ROOT: &str = "/sys/devices/system/cpu";

/// A set of CPUs sharing microarchitecture-visible properties.
#[derive(Debug, Clone)]
pub struct CoreClass {
    pub cpus: Vec<u32>,
    pub physical_cores: usize,
    pub smt: bool,
    pub max_khz: u32,
    pub l2_kb: u32,
    /// Groups of CPUs that share an L2. One entry per distinct L2 instance.
    pub l2_domains: Vec<Vec<u32>>,
    pub l3_kb: u32,
    /// Groups of CPUs that share an L3 — a CCX, on AMD.
    pub l3_domains: Vec<Vec<u32>>,
}

/// The axis on which two core classes differ. Reported rather than inferred,
/// because the answer is vendor-specific and a wrong guess here is invisible.
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord, Clone, Copy)]
pub enum Asymmetry {
    Frequency,
    Smt,
    L2Size,
    L3Size,
}

impl Asymmetry {
    pub fn as_str(self) -> &'static str {
        match self {
            Asymmetry::Frequency => "max frequency",
            Asymmetry::Smt => "SMT",
            Asymmetry::L2Size => "L2 size",
            Asymmetry::L3Size => "L3 size",
        }
    }
}

#[derive(Debug)]
pub struct CpuInventory {
    pub classes: Vec<CoreClass>,
    pub asymmetry: Vec<Asymmetry>,
    /// True when `cpu_capacity` exists and is identical across every CPU while
    /// `classes.len() > 1` — i.e. the attribute is actively misleading on this
    /// machine. Surfaced so the trap documented above is *observed* here rather
    /// than only described in a comment.
    pub capacity_is_misleading: bool,
    pub mem_total_bytes: Option<u64>,
}

/// One logical CPU as sysfs describes it.
struct Logical {
    id: u32,
    max_khz: u32,
    siblings: Vec<u32>,
    core_id: u32,
    l2_kb: u32,
    l2_shared: Vec<u32>,
    l3_kb: u32,
    l3_shared: Vec<u32>,
    capacity: Option<u64>,
}

fn online_cpus(root: &Path) -> Vec<u32> {
    // `present` rather than a glob of cpu*/ so that offline CPUs are still
    // counted: an inventory that changes shape when a core is offlined would
    // make any policy derived from it depend on when it ran.
    if let Some(s) = read(root.join("present")) {
        let cpus = parse_cpulist(&s);
        if !cpus.is_empty() {
            return cpus;
        }
    }
    Vec::new()
}

fn read_logical(root: &Path, id: u32) -> Option<Logical> {
    let d = root.join(format!("cpu{id}")).to_string_lossy().to_string();

    // A CPU with no cpufreq policy (no driver, or a VM) has no max frequency.
    // Zero is used as "unknown" and groups such CPUs together, which is the
    // correct outcome: without frequency they are indistinguishable on that
    // axis and must not be split by it.
    let max_khz = read_u64(format!("{d}/cpufreq/cpuinfo_max_freq")).unwrap_or(0) as u32;

    let siblings = read(format!("{d}/topology/core_cpus_list"))
        .or_else(|| read(format!("{d}/topology/thread_siblings_list")))
        .map(|s| parse_cpulist(&s))
        .unwrap_or_else(|| vec![id]);

    let core_id = read_u64(format!("{d}/topology/core_id")).unwrap_or(id as u64) as u32;

    // L2 and L3 both matter and for different reasons. L2 sharing distinguishes
    // an E-core cluster from a private cache; L3 *size* is what distinguishes a
    // stacked-cache CCX from its plain sibling on an X3D part, which is the AMD
    // asymmetry compute.md §4.1 requires this type to be able to hold.
    let mut l2_kb = 0;
    let mut l2_shared = vec![id];
    let mut l3_kb = 0;
    let mut l3_shared = vec![id];
    if let Ok(entries) = std::fs::read_dir(format!("{d}/cache")) {
        for e in entries.flatten() {
            let p = e.path();
            let size = read(p.join("size")).and_then(|s| parse_cache_kb(&s));
            let shared = read(p.join("shared_cpu_list"))
                .map(|s| parse_cpulist(&s))
                .filter(|v| !v.is_empty());
            match read(p.join("level")).as_deref() {
                Some("2") => {
                    if let Some(s) = size {
                        l2_kb = s;
                    }
                    if let Some(sh) = shared {
                        l2_shared = sh;
                    }
                }
                Some("3") => {
                    if let Some(s) = size {
                        l3_kb = s;
                    }
                    if let Some(sh) = shared {
                        l3_shared = sh;
                    }
                }
                _ => {}
            }
        }
    }

    Some(Logical {
        id,
        max_khz,
        siblings,
        core_id,
        l2_kb,
        l2_shared,
        l3_kb,
        l3_shared,
        capacity: read_u64(format!("{d}/cpu_capacity")),
    })
}

pub fn inventory() -> CpuInventory {
    inventory_at(Path::new(CPU_ROOT), Some(Path::new("/proc/meminfo")))
}

pub fn inventory_at(root: &Path, meminfo: Option<&Path>) -> CpuInventory {
    let logicals: Vec<Logical> = online_cpus(root)
        .into_iter()
        .filter_map(|id| read_logical(root, id))
        .collect();

    // Group on the four axes that actually vary. Deliberately *not* core_id or
    // cache instance — those distinguish siblings within a class, not classes.
    // L3 *size* is in the key but L3 *sharing* is not: two same-size CCXs are
    // interchangeable and must stay one class, while an X3D's stacked-cache CCX
    // is genuinely a different unit.
    let mut groups: BTreeMap<(u32, bool, u32, u32), Vec<&Logical>> = BTreeMap::new();
    for l in &logicals {
        let smt = l.siblings.len() > 1;
        groups
            .entry((l.max_khz, smt, l.l2_kb, l.l3_kb))
            .or_default()
            .push(l);
    }

    let mut classes: Vec<CoreClass> = groups
        .into_iter()
        .map(|((max_khz, smt, l2_kb, l3_kb), members)| {
            let cpus: Vec<u32> = members.iter().map(|l| l.id).collect();
            let physical_cores = members.iter().map(|l| l.core_id).collect::<BTreeSet<_>>().len();
            let l2_domains: BTreeSet<Vec<u32>> =
                members.iter().map(|l| l.l2_shared.clone()).collect();
            let l3_domains: BTreeSet<Vec<u32>> =
                members.iter().map(|l| l.l3_shared.clone()).collect();
            CoreClass {
                cpus,
                physical_cores,
                smt,
                max_khz,
                l2_kb,
                l2_domains: l2_domains.into_iter().collect(),
                l3_kb,
                l3_domains: l3_domains.into_iter().collect(),
            }
        })
        .collect();

    // Fastest first. A consumer that wants "the good cores" should not have to
    // know which vendor it is on to find them.
    classes.sort_by(|a, b| {
        b.max_khz
            .cmp(&a.max_khz)
            .then(b.l3_kb.cmp(&a.l3_kb))
            .then(b.l2_kb.cmp(&a.l2_kb))
            .then(a.cpus.first().cmp(&b.cpus.first()))
    });

    let mut asymmetry = Vec::new();
    if classes.len() > 1 {
        let distinct = |f: &dyn Fn(&CoreClass) -> u64| -> bool {
            classes.iter().map(f).collect::<BTreeSet<_>>().len() > 1
        };
        if distinct(&|c: &CoreClass| c.max_khz as u64) {
            asymmetry.push(Asymmetry::Frequency);
        }
        if distinct(&|c: &CoreClass| c.smt as u64) {
            asymmetry.push(Asymmetry::Smt);
        }
        if distinct(&|c: &CoreClass| c.l2_kb as u64) {
            asymmetry.push(Asymmetry::L2Size);
        }
        if distinct(&|c: &CoreClass| c.l3_kb as u64) {
            asymmetry.push(Asymmetry::L3Size);
        }
    }

    let capacities: BTreeSet<Option<u64>> = logicals.iter().map(|l| l.capacity).collect();
    let capacity_is_misleading = classes.len() > 1
        && capacities.len() == 1
        && capacities.iter().next().is_some_and(|c| c.is_some());

    CpuInventory {
        classes,
        asymmetry,
        capacity_is_misleading,
        mem_total_bytes: meminfo.and_then(mem_total),
    }
}

fn mem_total(path: &Path) -> Option<u64> {
    let s = std::fs::read_to_string(path).ok()?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("MemTotal:") {
            let kb: u64 = rest.trim().trim_end_matches(" kB").trim().parse().ok()?;
            return Some(kb * 1024);
        }
    }
    None
}

impl CoreClass {
    pub fn max_mhz(&self) -> u32 {
        self.max_khz / 1000
    }

    /// Threads per physical core, as observed rather than assumed to be 2.
    pub fn threads_per_core(&self) -> usize {
        self.cpus.len().checked_div(self.physical_cores).unwrap_or(0)
    }

    pub fn label(&self) -> String {
        format!(
            "{} core{} / {} thread{}",
            self.physical_cores,
            if self.physical_cores == 1 { "" } else { "s" },
            self.cpus.len(),
            if self.cpus.len() == 1 { "" } else { "s" }
        )
    }
}

pub fn to_json(inv: &CpuInventory) -> String {
    let classes: Vec<String> = inv
        .classes
        .iter()
        .map(|c| {
            let cpus: Vec<String> = c.cpus.iter().map(|n| n.to_string()).collect();
            let domains: Vec<String> = c
                .l2_domains
                .iter()
                .map(|d| {
                    let ids: Vec<String> = d.iter().map(|n| n.to_string()).collect();
                    format!("[{}]", ids.join(","))
                })
                .collect();
            format!(
                r#"{{"cpus":[{}],"physical_cores":{},"smt":{},"max_khz":{},"l2_kb":{},"l2_domains":[{}],"l3_kb":{}}}"#,
                cpus.join(","),
                c.physical_cores,
                c.smt,
                c.max_khz,
                c.l2_kb,
                domains.join(","),
                c.l3_kb
            )
        })
        .collect();

    let axes: Vec<String> = inv
        .asymmetry
        .iter()
        .map(|a| format!(r#""{}""#, sysfs::json_escape(a.as_str())))
        .collect();

    format!(
        r#"{{"classes":[{}],"asymmetry":[{}],"cpu_capacity_misleading":{},"mem_total_bytes":{}}}"#,
        classes.join(","),
        axes.join(","),
        inv.capacity_is_misleading,
        inv.mem_total_bytes
            .map(|b| b.to_string())
            .unwrap_or_else(|| "null".into())
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    /// This laptop, as measured 2026-08-23. The regression test for §4.1a: the
    /// only machine any of this was observed on.
    fn intel_hybrid() -> Fixture {
        let f = Fixture::new("cpu-adl");
        f.present("0-11");
        // 2 P-cores, SMT, 1280K private L2, 4.4 GHz.
        f.cpu(0, 4400000, "0-1", 0, 1280, "0-1", Some(1024));
        f.cpu(1, 4400000, "0-1", 0, 1280, "0-1", Some(1024));
        f.cpu(2, 4400000, "2-3", 4, 1280, "2-3", Some(1024));
        f.cpu(3, 4400000, "2-3", 4, 1280, "2-3", Some(1024));
        // 8 E-cores, no SMT, 2048K shared per cluster of four, 3.3 GHz.
        for (i, id) in (4..8u32).enumerate() {
            f.cpu(id, 3300000, &id.to_string(), 8 + i as u32, 2048, "4-7", Some(1024));
        }
        for (i, id) in (8..12u32).enumerate() {
            f.cpu(id, 3300000, &id.to_string(), 12 + i as u32, 2048, "8-11", Some(1024));
        }
        f
    }

    #[test]
    fn intel_hybrid_matches_the_measured_machine() {
        let f = intel_hybrid();
        let inv = inventory_at(f.path(), None);

        assert_eq!(inv.classes.len(), 2, "P and E must not collapse into one class");

        // Fastest first, so a caller need not know the vendor to find the good cores.
        let p = &inv.classes[0];
        assert_eq!(p.max_khz, 4400000);
        assert_eq!(p.cpus, vec![0, 1, 2, 3]);
        assert_eq!(p.physical_cores, 2);
        assert!(p.smt);
        assert_eq!(p.threads_per_core(), 2);
        assert_eq!(p.l2_domains, vec![vec![0, 1], vec![2, 3]]);

        let e = &inv.classes[1];
        assert_eq!(e.max_khz, 3300000);
        assert_eq!(e.cpus, (4..12).collect::<Vec<_>>());
        assert_eq!(e.physical_cores, 8);
        assert!(!e.smt);
        assert_eq!(e.l2_domains, vec![vec![4, 5, 6, 7], vec![8, 9, 10, 11]]);

        assert_eq!(
            inv.asymmetry,
            vec![Asymmetry::Frequency, Asymmetry::Smt, Asymmetry::L2Size]
        );
    }

    /// §4.1a. The trap: cpu_capacity reads 1024 on every CPU of a hybrid part,
    /// so code that routes on it sees a homogeneous machine.
    #[test]
    fn uniform_cpu_capacity_is_flagged_on_a_heterogeneous_machine() {
        let f = intel_hybrid();
        let inv = inventory_at(f.path(), None);
        assert!(inv.capacity_is_misleading);
    }

    #[test]
    fn absent_cpu_capacity_is_not_flagged_as_misleading() {
        let f = Fixture::new("cpu-nocap");
        f.present("0-3");
        f.cpu(0, 4400000, "0-1", 0, 1280, "0-1", None);
        f.cpu(1, 4400000, "0-1", 0, 1280, "0-1", None);
        f.cpu(2, 3300000, "2", 2, 2048, "2-3", None);
        f.cpu(3, 3300000, "3", 3, 2048, "2-3", None);
        let inv = inventory_at(f.path(), None);
        assert_eq!(inv.classes.len(), 2);
        // Absent is honest; only present-and-uniform is a trap worth naming.
        assert!(!inv.capacity_is_misleading);
    }

    /// A homogeneous CPU must produce exactly one class, or every consumer has
    /// to special-case "classes that are all the same".
    #[test]
    fn homogeneous_cpu_is_one_class_with_no_asymmetry() {
        let f = Fixture::new("cpu-uniform");
        f.present("0-7");
        for id in 0..8u32 {
            f.cpu(id, 3600000, &format!("{}-{}", id & !1, (id & !1) + 1), id / 2, 512, "0-7", None);
        }
        let inv = inventory_at(f.path(), None);
        assert_eq!(inv.classes.len(), 1);
        assert!(inv.asymmetry.is_empty());
        assert!(!inv.capacity_is_misleading);
    }

    /// docs/compute.md §4.1: the AMD case the original `CpuCcx` sketch was
    /// written for. 8 cores / 16 threads, one CCX, a large shared L3.
    ///
    /// Honest limit: this layout is inferred, not observed — no AMD machine has
    /// run sonar. What it tests is that a single-CCX part collapses to one
    /// class and does not get split by cache instance.
    #[test]
    fn amd_single_ccx_is_one_class() {
        let f = Fixture::new("cpu-amd-1ccx");
        f.present("0-15");
        for id in 0..16u32 {
            let core = id / 2;
            let sibs = format!("{}-{}", core * 2, core * 2 + 1);
            f.cpu(id, 5200000, &sibs, core, 1024, &sibs, None);
            f.cpu_l3(id, 98304, "0-15");
        }
        let inv = inventory_at(f.path(), None);
        assert_eq!(inv.classes.len(), 1, "one CCX is one class");
        let c = &inv.classes[0];
        assert_eq!(c.physical_cores, 8);
        assert_eq!(c.cpus.len(), 16);
        assert!(c.smt);
        assert_eq!(c.l2_domains.len(), 8, "private L2 per core, not one shared");
        assert_eq!(c.l3_kb, 98304);
        assert_eq!(c.l3_domains, vec![(0..16).collect::<Vec<u32>>()]);
    }

    /// The X3D asymmetry: two CCXs at the same frequency and SMT width, one
    /// with a much larger L3. This is the case the original `CpuCcx` sketch was
    /// named for, and the case a (freq, smt, L2) grouping key cannot see.
    ///
    /// Honest limit: the layout is inferred from how X3D parts are described,
    /// not observed — no AMD machine has run sonar.
    #[test]
    fn dual_ccx_differing_only_in_l3_splits_into_two_classes() {
        let f = Fixture::new("cpu-amd-x3d");
        f.present("0-15");
        for id in 0..16u32 {
            let core = id / 2;
            let sibs = format!("{}-{}", core * 2, core * 2 + 1);
            f.cpu(id, 5200000, &sibs, core, 1024, &sibs, None);
            // CCX0 carries stacked cache; CCX1 does not.
            if id < 8 {
                f.cpu_l3(id, 98304, "0-7");
            } else {
                f.cpu_l3(id, 32768, "8-15");
            }
        }
        let inv = inventory_at(f.path(), None);
        assert_eq!(
            inv.classes.len(),
            2,
            "an L3-only asymmetry must not collapse — this is the X3D case"
        );
        // Larger cache sorts first, so "the good cores" is classes[0] without
        // the caller knowing which vendor it is on.
        assert_eq!(inv.classes[0].l3_kb, 98304);
        assert_eq!(inv.classes[0].cpus, (0..8).collect::<Vec<_>>());
        assert_eq!(inv.classes[1].l3_kb, 32768);
        assert_eq!(inv.classes[1].cpus, (8..16).collect::<Vec<_>>());
        assert_eq!(inv.asymmetry, vec![Asymmetry::L3Size]);
        assert_eq!(inv.classes[0].l3_domains, vec![(0..8).collect::<Vec<u32>>()]);
    }

    /// The other half of the same rule: two CCXs of equal size are
    /// interchangeable and must stay one class, even though they are distinct
    /// cache domains. Splitting here would report a plain Ryzen as
    /// heterogeneous and send every consumer down a routing path for nothing.
    #[test]
    fn dual_ccx_of_equal_size_stays_one_class_with_two_domains() {
        let f = Fixture::new("cpu-amd-2ccx");
        f.present("0-15");
        for id in 0..16u32 {
            let core = id / 2;
            let sibs = format!("{}-{}", core * 2, core * 2 + 1);
            f.cpu(id, 5200000, &sibs, core, 1024, &sibs, None);
            f.cpu_l3(id, 32768, if id < 8 { "0-7" } else { "8-15" });
        }
        let inv = inventory_at(f.path(), None);
        assert_eq!(inv.classes.len(), 1);
        assert!(inv.asymmetry.is_empty());
        assert_eq!(
            inv.classes[0].l3_domains,
            vec![(0..8).collect::<Vec<u32>>(), (8..16).collect::<Vec<u32>>()],
            "cache locality is still visible as two domains within the class"
        );
    }

    #[test]
    fn missing_cpufreq_groups_together_rather_than_splitting_on_zero() {
        let f = Fixture::new("cpu-nofreq");
        f.present("0-3");
        for id in 0..4u32 {
            let d = format!("cpu{id}");
            // No cpufreq directory at all, as in a VM.
            f.present("0-3");
            std::fs::create_dir_all(f.path().join(format!("{d}/topology"))).unwrap();
            std::fs::write(f.path().join(format!("{d}/topology/core_cpus_list")), format!("{id}\n")).unwrap();
            std::fs::write(f.path().join(format!("{d}/topology/core_id")), format!("{id}\n")).unwrap();
        }
        let inv = inventory_at(f.path(), None);
        assert_eq!(inv.classes.len(), 1);
        assert_eq!(inv.classes[0].max_khz, 0);
        assert!(inv.asymmetry.is_empty());
    }

    #[test]
    fn absent_cpu_root_yields_no_classes() {
        let inv = inventory_at(Path::new("/nonexistent/cpu"), None);
        assert!(inv.classes.is_empty());
    }
}
