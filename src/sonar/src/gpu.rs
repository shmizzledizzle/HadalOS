//! GPU inventory, and the one method docs/compute.md §4.1b exists to justify.
//!
//! `contends_with_display` is the reason the GPU record is not a device-class
//! constant. An integrated GPU shares memory bandwidth *and* execution units
//! with whatever is drawing the screen; a discrete GPU that is not driving a
//! display does not. Same question, opposite answers, decided per device.
//!
//! # Integrated vs discrete is derived, and can fail to be known
//!
//! Nothing in sysfs says "discrete". What distinguishes them is dedicated
//! memory, under a per-driver attribute name:
//!
//! - `amdgpu` → `mem_info_vram_total`
//! - `i915` / `xe` on discrete parts → `lmem_total_bytes`
//!
//! The first version of this file treated *absence* of both as "integrated".
//! That is wrong, and docs/compute.md §6a predicted where: **nouveau exposes
//! neither**, so a Quadro K2200 would be reported as integrated, confidently,
//! in one line — §4.1a's failure mode in the code that section congratulates
//! itself for avoiding.
//!
//! So absence is only meaningful for a driver that *would* have reported
//! dedicated memory if it had any. For anything else the answer is `Unknown`,
//! and `Unknown` is treated as contending, because assuming a GPU we cannot
//! classify does not contend with the display is the unsafe direction — the
//! same "when the safe action is unavailable, decline" shape as tier-routing
//! §4 and the Limine `lastgood` pin.

use crate::sysfs::{self, read, read_u64};
use std::fs;
use std::path::{Path, PathBuf};

pub const DRM_ROOT: &str = "/sys/class/drm";

/// Drivers that expose a dedicated-memory attribute whenever they have
/// dedicated memory. For these, and only these, absence means integrated.
///
/// Adding a driver here is a claim about that driver's sysfs surface. Getting
/// it wrong reintroduces the nouveau bug for a different device, so the bar is
/// having seen the attribute present on a discrete part of that driver.
const DRIVERS_REPORTING_VRAM: &[&str] = &["i915", "xe", "amdgpu"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GpuMemory {
    /// Dedicated memory of known size. Discrete.
    Dedicated(u64),
    /// No dedicated memory, from a driver that would have said so. Integrated.
    Shared,
    /// This driver exposes no dedicated-memory attribute, so absence says
    /// nothing about the device. Not a synonym for `Shared`.
    Unknown,
}

#[derive(Debug, Clone)]
pub struct Gpu {
    /// `card0`, `card1`, …
    pub card: String,
    pub driver: Option<String>,
    pub pci_id: Option<String>,
    pub memory: GpuMemory,
    /// Connectors on this card reporting `connected`.
    pub connected_outputs: Vec<String>,
    pub has_render_node: bool,
    pub numa_node: Option<i32>,
}

impl Gpu {
    pub fn drives_display(&self) -> bool {
        !self.connected_outputs.is_empty()
    }

    /// docs/compute.md §4.1b. Integrated always contends — it shares the memory
    /// bus with the display controller whether or not this card has a connected
    /// output. Discrete contends only while it is driving one. Unknown contends,
    /// because the alternative is a silent optimistic guess.
    pub fn contends_with_display(&self) -> bool {
        match self.memory {
            GpuMemory::Shared => true,
            GpuMemory::Dedicated(_) => self.drives_display(),
            GpuMemory::Unknown => true,
        }
    }

    pub fn kind(&self) -> &'static str {
        match self.memory {
            GpuMemory::Dedicated(_) => "discrete",
            GpuMemory::Shared => "integrated",
            GpuMemory::Unknown => "unknown",
        }
    }

    /// What to print about this device's memory, including *why* it is unknown
    /// when it is. An inventory that prints "unknown" without saying what it
    /// tried is not actionable.
    pub fn memory_note(&self) -> String {
        match self.memory {
            GpuMemory::Dedicated(b) => format!("VRAM {}", sysfs::fmt_bytes(b)),
            GpuMemory::Shared => "shares system memory".to_string(),
            GpuMemory::Unknown => format!(
                "cannot tell — {} exposes no dedicated-memory attribute",
                self.driver.as_deref().unwrap_or("this driver")
            ),
        }
    }
}

/// `card0` yes; `card0-eDP-1` no. The connector entries live in the same
/// directory and share the prefix, so the check is that everything after
/// "card" is digits — a `starts_with("card")` filter picks up every connector.
fn is_card_dir(name: &str) -> bool {
    match name.strip_prefix("card") {
        Some(rest) => !rest.is_empty() && rest.bytes().all(|b| b.is_ascii_digit()),
        None => false,
    }
}

fn connected_outputs(root: &Path, card: &str) -> Vec<String> {
    let mut out = Vec::new();
    let prefix = format!("{card}-");
    if let Ok(entries) = fs::read_dir(root) {
        for e in entries.flatten() {
            let name = e.file_name().to_string_lossy().to_string();
            let Some(conn) = name.strip_prefix(&prefix) else {
                continue;
            };
            // "connected"; explicitly not "unknown", which some drivers report
            // for a connector they cannot probe. Counting unknown as connected
            // would mark a headless discrete GPU as contending forever.
            if read(e.path().join("status")).as_deref() == Some("connected") {
                out.push(conn.to_string());
            }
        }
    }
    out.sort();
    out
}

fn classify_memory(dev: &Path, driver: Option<&str>) -> GpuMemory {
    if let Some(b) = read_u64(dev.join("mem_info_vram_total"))
        .or_else(|| read_u64(dev.join("lmem_total_bytes")))
    {
        return GpuMemory::Dedicated(b);
    }
    match driver {
        Some(d) if DRIVERS_REPORTING_VRAM.contains(&d) => GpuMemory::Shared,
        _ => GpuMemory::Unknown,
    }
}

fn render_node_for(root: &Path, card: &str) -> bool {
    // The render node is what a compute client actually opens. A card without
    // one cannot be dispatched to, which is worth knowing at inventory time
    // rather than at first use.
    let Some(n) = card.strip_prefix("card").and_then(|n| n.parse::<u32>().ok()) else {
        return false;
    };
    root.join(format!("renderD{}", 128 + n)).exists()
}

pub fn inventory() -> Vec<Gpu> {
    inventory_at(Path::new(DRM_ROOT))
}

pub fn inventory_at(root: &Path) -> Vec<Gpu> {
    let mut gpus = Vec::new();
    let Ok(entries) = fs::read_dir(root) else {
        return gpus;
    };

    for e in entries.flatten() {
        let card = e.file_name().to_string_lossy().to_string();
        if !is_card_dir(&card) {
            continue;
        }
        let dev: PathBuf = e.path().join("device");

        let driver = fs::read_link(dev.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().to_string()));

        // `device/device` is the PCI device id; pair it with the vendor for a
        // value that can be matched against lspci output by a human.
        let pci_id = match (read(dev.join("vendor")), read(dev.join("device"))) {
            (Some(v), Some(d)) => Some(format!(
                "{}:{}",
                v.trim_start_matches("0x"),
                d.trim_start_matches("0x")
            )),
            _ => None,
        };

        gpus.push(Gpu {
            memory: classify_memory(&dev, driver.as_deref()),
            driver,
            pci_id,
            connected_outputs: connected_outputs(root, &card),
            has_render_node: render_node_for(root, &card),
            numa_node: read(dev.join("numa_node")).and_then(|s| s.parse().ok()),
            card,
        });
    }

    gpus.sort_by(|a, b| a.card.cmp(&b.card));
    gpus
}

/// Vulkan ICDs installed on this system.
///
/// This reports what Mesa *installed*, which is not the same as what can run
/// here — the reference laptop carries `radeon_icd` with no AMD GPU present,
/// because Mesa builds RADV and ANV together. Verifying a driver loads and
/// dispatches is `probe/vkprobe.c`, which is a separate program precisely
/// because it is a separate claim.
pub fn vulkan_icds() -> Vec<String> {
    let mut out = Vec::new();
    for dir in ["/usr/share/vulkan/icd.d", "/etc/vulkan/icd.d"] {
        if let Ok(entries) = fs::read_dir(dir) {
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().to_string();
                // One ICD per architecture is installed; the i686 copy is not a
                // separate driver and listing it doubles the output for nothing.
                if name.ends_with(".json") && !name.contains("i686") {
                    out.push(name);
                }
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

pub fn to_json(gpus: &[Gpu], icds: &[String]) -> String {
    let items: Vec<String> = gpus
        .iter()
        .map(|g| {
            let outs: Vec<String> = g
                .connected_outputs
                .iter()
                .map(|o| format!(r#""{}""#, sysfs::json_escape(o)))
                .collect();
            let quoted = |v: &Option<String>| {
                v.as_ref()
                    .map(|s| format!(r#""{}""#, sysfs::json_escape(s)))
                    .unwrap_or_else(|| "null".into())
            };
            let vram = match g.memory {
                GpuMemory::Dedicated(b) => b.to_string(),
                _ => "null".to_string(),
            };
            format!(
                r#"{{"card":"{}","driver":{},"pci_id":{},"kind":"{}","vram_bytes":{},"connected_outputs":[{}],"has_render_node":{},"numa_node":{},"contends_with_display":{}}}"#,
                sysfs::json_escape(&g.card),
                quoted(&g.driver),
                quoted(&g.pci_id),
                g.kind(),
                vram,
                outs.join(","),
                g.has_render_node,
                g.numa_node
                    .map(|n| n.to_string())
                    .unwrap_or_else(|| "null".into()),
                g.contends_with_display()
            )
        })
        .collect();

    let icd_items: Vec<String> = icds
        .iter()
        .map(|i| format!(r#""{}""#, sysfs::json_escape(i)))
        .collect();

    format!(
        r#"{{"gpus":[{}],"vulkan_icds_installed":[{}]}}"#,
        items.join(","),
        icd_items.join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::Fixture;

    #[test]
    fn card_dirs_are_distinguished_from_connectors() {
        assert!(is_card_dir("card0"));
        assert!(is_card_dir("card12"));
        // The bug this guards: a starts_with("card") filter treats every
        // connector as a GPU, and this laptop would report seven.
        assert!(!is_card_dir("card0-eDP-1"));
        assert!(!is_card_dir("card0-DP-1"));
        assert!(!is_card_dir("renderD128"));
        assert!(!is_card_dir("card"));
        assert!(!is_card_dir("version"));
    }

    fn gpu(memory: GpuMemory, outs: &[&str]) -> Gpu {
        Gpu {
            card: "card0".into(),
            driver: Some("test".into()),
            pci_id: None,
            memory,
            connected_outputs: outs.iter().map(|s| s.to_string()).collect(),
            has_render_node: true,
            numa_node: None,
        }
    }

    #[test]
    fn integrated_contends_even_with_no_output() {
        assert!(gpu(GpuMemory::Shared, &[]).contends_with_display());
        assert!(gpu(GpuMemory::Shared, &["eDP-1"]).contends_with_display());
    }

    #[test]
    fn discrete_contends_only_while_driving_a_display() {
        assert!(!gpu(GpuMemory::Dedicated(16 << 30), &[]).contends_with_display());
        assert!(gpu(GpuMemory::Dedicated(16 << 30), &["DP-1"]).contends_with_display());
    }

    #[test]
    fn unknown_contends_because_guessing_otherwise_is_the_unsafe_direction() {
        assert!(gpu(GpuMemory::Unknown, &[]).contends_with_display());
    }

    // ---- fixture-backed: hardware this machine does not have ----

    #[test]
    fn intel_igpu_this_laptop() {
        let f = Fixture::new("gpu-i915");
        f.gpu_card(0, "i915", "0x8086", "0x46a8", None);
        f.connector("card0", "eDP-1", "connected");
        f.connector("card0", "HDMI-A-1", "disconnected");
        f.render_node(0);

        let g = inventory_at(f.path());
        assert_eq!(g.len(), 1, "one card, not one per connector");
        assert_eq!(g[0].memory, GpuMemory::Shared);
        assert_eq!(g[0].kind(), "integrated");
        assert_eq!(g[0].connected_outputs, vec!["eDP-1"]);
        assert!(g[0].has_render_node);
        assert!(g[0].contends_with_display());
    }

    #[test]
    fn amd_discrete_headless_does_not_contend() {
        let f = Fixture::new("gpu-amdgpu");
        f.gpu_card(0, "amdgpu", "0x1002", "0x7550", Some(16_106_127_360));
        f.connector("card0", "DP-1", "disconnected");
        f.render_node(0);

        let g = inventory_at(f.path());
        assert_eq!(g[0].memory, GpuMemory::Dedicated(16_106_127_360));
        assert_eq!(g[0].kind(), "discrete");
        assert!(
            !g[0].contends_with_display(),
            "a discrete card driving nothing must not reserve display budget"
        );
    }

    /// docs/compute.md §6a. This is the regression test for the bug the tower
    /// would have found: nouveau exposes no dedicated-memory attribute, and the
    /// first version of this file called that "integrated".
    #[test]
    fn nouveau_discrete_is_unknown_not_integrated() {
        let f = Fixture::new("gpu-nouveau");
        f.gpu_card(0, "nouveau", "0x10de", "0x13ba", None);
        f.connector("card0", "DP-1", "connected");
        f.render_node(0);

        let g = inventory_at(f.path());
        assert_eq!(
            g[0].memory,
            GpuMemory::Unknown,
            "absence of the attribute must not be read as 'integrated'"
        );
        assert_eq!(g[0].kind(), "unknown");
        assert!(g[0].memory_note().contains("nouveau"));
        assert!(g[0].contends_with_display());
    }

    #[test]
    fn hybrid_igpu_plus_discrete_is_ordered_and_classified_separately() {
        let f = Fixture::new("gpu-hybrid");
        f.gpu_card(0, "i915", "0x8086", "0x46a8", None);
        f.connector("card0", "eDP-1", "connected");
        f.render_node(0);
        f.gpu_card(1, "amdgpu", "0x1002", "0x7550", Some(8 << 30));
        f.connector("card1", "DP-1", "disconnected");
        f.render_node(1);

        let g = inventory_at(f.path());
        assert_eq!(g.len(), 2);
        assert_eq!(g[0].card, "card0");
        assert_eq!(g[0].kind(), "integrated");
        assert!(g[0].contends_with_display());
        assert_eq!(g[1].card, "card1");
        assert_eq!(g[1].kind(), "discrete");
        assert!(!g[1].contends_with_display());
    }

    #[test]
    fn connector_status_unknown_is_not_connected() {
        let f = Fixture::new("gpu-unknown-conn");
        f.gpu_card(0, "amdgpu", "0x1002", "0x7550", Some(8 << 30));
        f.connector("card0", "DP-1", "unknown");
        f.render_node(0);

        let g = inventory_at(f.path());
        assert!(g[0].connected_outputs.is_empty());
        assert!(!g[0].contends_with_display());
    }

    #[test]
    fn missing_render_node_is_reported() {
        let f = Fixture::new("gpu-no-render");
        f.gpu_card(0, "amdgpu", "0x1002", "0x7550", Some(8 << 30));
        let g = inventory_at(f.path());
        assert!(!g[0].has_render_node);
    }

    #[test]
    fn absent_drm_root_is_empty_not_a_panic() {
        assert!(inventory_at(Path::new("/nonexistent/drm")).is_empty());
    }
}
